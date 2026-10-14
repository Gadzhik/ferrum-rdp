/*
 * ferrum shim: FreeRDP 3 client core exposed through ferrum_shim.h.
 * Modeled on the upstream Wayland/X11 clients.
 */
#include "ferrum_shim.h"

#include <stdlib.h>
#include <string.h>

#include <winpr/crt.h>
#include <winpr/synch.h>
#include <winpr/clipboard.h>
#include <winpr/collections.h>
#include <winpr/string.h>

#include <freerdp/freerdp.h>
#include <freerdp/client.h>
#include <freerdp/client/cmdline.h>
#include <freerdp/client/channels.h>
#include <freerdp/client/cliprdr.h>
#include <freerdp/client/client_cliprdr_file.h>
#include <freerdp/client/disp.h>
#include <freerdp/client/rail.h>
#include <freerdp/channels/channels.h>
#include <freerdp/channels/cliprdr.h>
#include <freerdp/channels/disp.h>
#include <freerdp/channels/rail.h>
#include <freerdp/gdi/gdi.h>
#include <freerdp/gdi/gfx.h>
#include <freerdp/graphics.h>
#include <freerdp/codec/color.h>
#include <freerdp/utils/cliprdr_utils.h>
#include <freerdp/rail.h>
#include <freerdp/window.h>
#include <freerdp/log.h>
#include <freerdp/scancode.h>

#define TAG CLIENT_TAG("ferrum")

typedef struct
{
	char* mime;
	UINT32 format_id;
	UINT64 request_id;
} clip_request;

struct ferrum_session
{
	rdpContext* context;
	ferrum_callbacks cb;

	/* guards the fields below against concurrent access from UI threads */
	CRITICAL_SECTION lock;
	BOOL active;
	DispClientContext* disp;
	BOOL disp_activated;
	RailClientContext* rail;

	/* clipboard */
	CliprdrClientContext* cliprdr;
	wClipboard* system;
	CliprdrFileContext* file;
	CRITICAL_SECTION clip_lock;
	CLIPRDR_FORMAT* server_formats;
	size_t n_server_formats;
	CLIPRDR_FORMAT* client_formats;
	size_t n_client_formats;
	char** client_mimes;
	size_t n_client_mimes;
	wQueue* requests;
	BOOL clip_ready;

	/* rail icon cache: cacheId * 256 + entry -> converted icon */
	wHashTable* icons;
};

typedef struct
{
	rdpClientContext common;
	ferrum_session* s;
} ferrum_context;

typedef struct
{
	rdpPointer pointer;
	BYTE* data;
} ferrum_pointer;

typedef struct
{
	UINT32 width, height;
	BYTE* bgra;
} ferrum_icon;

static ferrum_session* get_session(rdpContext* context)
{
	if (!context)
		return nullptr;
	return ((ferrum_context*)context)->s;
}

/* ------------------------------------------------------------------ */
/* memory helpers                                                     */
/* ------------------------------------------------------------------ */

void* ferrum_malloc(size_t size)
{
	return malloc(size);
}

char* ferrum_strdup(const char* str)
{
	return str ? _strdup(str) : nullptr;
}

void ferrum_free(void* ptr)
{
	free(ptr);
}

const char* ferrum_error_name(uint32_t code)
{
	return freerdp_get_last_error_name(code);
}

const char* ferrum_error_string(uint32_t code)
{
	return freerdp_get_last_error_string(code);
}

/* ------------------------------------------------------------------ */
/* graphics                                                           */
/* ------------------------------------------------------------------ */

static void emit_full_frame(ferrum_session* s)
{
	rdpGdi* gdi = s->context->gdi;
	if (!gdi || !s->cb.on_frame)
		return;
	const int32_t rect[4] = { 0, 0, gdi->width, gdi->height };
	s->cb.on_frame(s->cb.user, gdi->primary_buffer, gdi->stride, (uint32_t)gdi->width,
	               (uint32_t)gdi->height, rect, 1);
}

static BOOL ferrum_end_paint(rdpContext* context)
{
	ferrum_session* s = get_session(context);
	rdpGdi* gdi = context->gdi;
	if (!s || !gdi || !gdi->primary)
		return FALSE;

	HGDI_DC hdc = gdi->primary->hdc;
	if (!hdc || !hdc->hwnd)
		return TRUE;

	HGDI_WND hwnd = hdc->hwnd;
	if (!hwnd->invalid || hwnd->invalid->null)
		return TRUE;

	const INT32 count = hwnd->ninvalid;
	if (s->cb.on_frame)
	{
		if (count > 0 && count <= 512)
		{
			int32_t rects[512 * 4];
			size_t n = 0;
			for (INT32 i = 0; i < count; i++)
			{
				const GDI_RGN* r = &hwnd->cinvalid[i];
				if (r->w <= 0 || r->h <= 0)
					continue;
				rects[n * 4 + 0] = r->x;
				rects[n * 4 + 1] = r->y;
				rects[n * 4 + 2] = r->w;
				rects[n * 4 + 3] = r->h;
				n++;
			}
			if (n > 0)
				s->cb.on_frame(s->cb.user, gdi->primary_buffer, gdi->stride, (uint32_t)gdi->width,
				               (uint32_t)gdi->height, rects, n);
		}
		else
		{
			const int32_t rect[4] = { hwnd->invalid->x, hwnd->invalid->y, hwnd->invalid->w,
				                      hwnd->invalid->h };
			s->cb.on_frame(s->cb.user, gdi->primary_buffer, gdi->stride, (uint32_t)gdi->width,
			               (uint32_t)gdi->height, rect, 1);
		}
	}

	hwnd->invalid->null = TRUE;
	hwnd->ninvalid = 0;
	return TRUE;
}

static BOOL ferrum_desktop_resize(rdpContext* context)
{
	ferrum_session* s = get_session(context);
	rdpSettings* settings = context->settings;
	const UINT32 w = freerdp_settings_get_uint32(settings, FreeRDP_DesktopWidth);
	const UINT32 h = freerdp_settings_get_uint32(settings, FreeRDP_DesktopHeight);

	if (!gdi_resize(context->gdi, w, h))
		return FALSE;

	if (s && s->cb.on_desktop_resize)
		s->cb.on_desktop_resize(s->cb.user, w, h);
	if (s)
		emit_full_frame(s);
	return TRUE;
}

/* ---- pointer ---- */

static BOOL ferrum_pointer_new(rdpContext* context, rdpPointer* pointer)
{
	ferrum_pointer* ptr = (ferrum_pointer*)pointer;
	const size_t size = 4ULL * pointer->width * pointer->height;
	ptr->data = winpr_aligned_malloc(size ? size : 4, 16);
	if (!ptr->data)
		return FALSE;

	if (!freerdp_image_copy_from_pointer_data(
	        ptr->data, PIXEL_FORMAT_BGRA32, 0, 0, 0, pointer->width, pointer->height,
	        pointer->xorMaskData, pointer->lengthXorMask, pointer->andMaskData,
	        pointer->lengthAndMask, pointer->xorBpp, &context->gdi->palette))
	{
		winpr_aligned_free(ptr->data);
		ptr->data = nullptr;
		return FALSE;
	}
	return TRUE;
}

static void ferrum_pointer_free(WINPR_ATTR_UNUSED rdpContext* context, rdpPointer* pointer)
{
	ferrum_pointer* ptr = (ferrum_pointer*)pointer;
	if (ptr)
	{
		winpr_aligned_free(ptr->data);
		ptr->data = nullptr;
	}
}

static BOOL ferrum_pointer_set(rdpContext* context, rdpPointer* pointer)
{
	ferrum_session* s = get_session(context);
	ferrum_pointer* ptr = (ferrum_pointer*)pointer;
	if (s && s->cb.on_cursor && ptr->data)
		s->cb.on_cursor(s->cb.user, FERRUM_CURSOR_BITMAP, ptr->data, pointer->width,
		                pointer->height, pointer->xPos, pointer->yPos);
	return TRUE;
}

static BOOL ferrum_pointer_set_null(rdpContext* context)
{
	ferrum_session* s = get_session(context);
	if (s && s->cb.on_cursor)
		s->cb.on_cursor(s->cb.user, FERRUM_CURSOR_HIDDEN, nullptr, 0, 0, 0, 0);
	return TRUE;
}

static BOOL ferrum_pointer_set_default(rdpContext* context)
{
	ferrum_session* s = get_session(context);
	if (s && s->cb.on_cursor)
		s->cb.on_cursor(s->cb.user, FERRUM_CURSOR_DEFAULT, nullptr, 0, 0, 0, 0);
	return TRUE;
}

static BOOL ferrum_pointer_set_position(rdpContext* context, UINT32 x, UINT32 y)
{
	ferrum_session* s = get_session(context);
	if (s && s->cb.on_cursor_position)
		s->cb.on_cursor_position(s->cb.user, x, y);
	return TRUE;
}

static void register_pointer(rdpGraphics* graphics)
{
	rdpPointer pointer = WINPR_C_ARRAY_INIT;
	pointer.size = sizeof(ferrum_pointer);
	pointer.New = ferrum_pointer_new;
	pointer.Free = ferrum_pointer_free;
	pointer.Set = ferrum_pointer_set;
	pointer.SetNull = ferrum_pointer_set_null;
	pointer.SetDefault = ferrum_pointer_set_default;
	pointer.SetPosition = ferrum_pointer_set_position;
	graphics_register_pointer(graphics, &pointer);
}

/* ------------------------------------------------------------------ */
/* display control                                                    */
/* ------------------------------------------------------------------ */

static UINT ferrum_disp_caps(DispClientContext* disp, WINPR_ATTR_UNUSED UINT32 maxNumMonitors,
                             WINPR_ATTR_UNUSED UINT32 maxMonitorAreaFactorA,
                             WINPR_ATTR_UNUSED UINT32 maxMonitorAreaFactorB)
{
	ferrum_session* s = disp->custom;
	if (s)
	{
		EnterCriticalSection(&s->lock);
		s->disp_activated = TRUE;
		LeaveCriticalSection(&s->lock);
	}
	return CHANNEL_RC_OK;
}

static UINT32 orientation_from_degrees(UINT32 deg)
{
	switch (deg)
	{
		case 90:
			return ORIENTATION_PORTRAIT;
		case 180:
			return ORIENTATION_LANDSCAPE_FLIPPED;
		case 270:
			return ORIENTATION_PORTRAIT_FLIPPED;
		default:
			return ORIENTATION_LANDSCAPE;
	}
}

void ferrum_display_layout(ferrum_session* s, const ferrum_monitor* monitors, size_t count)
{
	if (!s || !monitors || count == 0 || count > 16)
		return;

	DISPLAY_CONTROL_MONITOR_LAYOUT layouts[16] = WINPR_C_ARRAY_INIT;
	for (size_t i = 0; i < count; i++)
	{
		const ferrum_monitor* m = &monitors[i];
		DISPLAY_CONTROL_MONITOR_LAYOUT* l = &layouts[i];
		/* MS-RDPEDISP: width must be even and within 200..8192 */
		UINT32 w = (UINT32)MAX(200, MIN(8192, m->width)) & ~1u;
		UINT32 h = (UINT32)MAX(200, MIN(8192, m->height));
		l->Flags = m->primary ? DISPLAY_CONTROL_MONITOR_PRIMARY : 0;
		l->Left = m->x;
		l->Top = m->y;
		l->Width = w;
		l->Height = h;
		l->PhysicalWidth = m->physical_width_mm;
		l->PhysicalHeight = m->physical_height_mm;
		l->Orientation = orientation_from_degrees(m->orientation);
		l->DesktopScaleFactor = m->desktop_scale ? m->desktop_scale : 100;
		l->DeviceScaleFactor = m->device_scale ? m->device_scale : 100;
	}

	EnterCriticalSection(&s->lock);
	if (s->active && s->disp && s->disp_activated && s->disp->SendMonitorLayout)
		s->disp->SendMonitorLayout(s->disp, (UINT32)count, layouts);
	LeaveCriticalSection(&s->lock);
}

void ferrum_display_resize(ferrum_session* s, uint32_t width, uint32_t height, uint32_t scale)
{
	if (!s)
		return;
	ferrum_monitor m = { 0 };
	m.width = (int32_t)width;
	m.height = (int32_t)height;
	m.primary = true;
	m.desktop_scale = scale ? scale : 100;
	m.device_scale = scale >= 180 ? 180 : (scale >= 140 ? 140 : 100);
	/* ~96 dpi physical size, servers mostly ignore it but it must be sane */
	m.physical_width_mm = width * 254 / 960;
	m.physical_height_mm = height * 254 / 960;
	ferrum_display_layout(s, &m, 1);
}

void ferrum_refresh(ferrum_session* s)
{
	if (!s)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active)
	{
		rdpGdi* gdi = s->context->gdi;
		if (gdi)
		{
			RECTANGLE_16 r = { 0, 0, (UINT16)gdi->width, (UINT16)gdi->height };
			IFCALL(s->context->update->RefreshRect, s->context, 1, &r);
		}
	}
	LeaveCriticalSection(&s->lock);
}

void ferrum_suppress_output(ferrum_session* s, bool suppress)
{
	if (!s)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active)
	{
		rdpGdi* gdi = s->context->gdi;
		if (gdi)
		{
			RECTANGLE_16 r = { 0, 0, (UINT16)gdi->width, (UINT16)gdi->height };
			IFCALL(s->context->update->SuppressOutput, s->context, suppress ? 0 : 1, &r);
		}
	}
	LeaveCriticalSection(&s->lock);
}

/* ------------------------------------------------------------------ */
/* clipboard                                                          */
/* ------------------------------------------------------------------ */

static const char mime_text_utf8[] = "text/plain;charset=utf-8";
static const char mime_text_plain[] = "text/plain";
static const char mime_html[] = "text/html";
static const char mime_png[] = "image/png";
static const char mime_bmp[] = "image/bmp";
static const char mime_uri_list[] = "text/uri-list";
static const char mime_gnome_files[] = "x-special/gnome-copied-files";
static const char type_file_group[] = "FileGroupDescriptorW";
static const char type_html[] = "HTML Format";

static BOOL mime_is_text(const char* m)
{
	return strcmp(m, mime_text_utf8) == 0 || strcmp(m, mime_text_plain) == 0 ||
	       strcmp(m, "UTF8_STRING") == 0 || strcmp(m, "STRING") == 0 || strcmp(m, "TEXT") == 0;
}

static BOOL mime_is_image(const char* m)
{
	return strncmp(m, "image/", 6) == 0;
}

static BOOL mime_is_file(const char* m)
{
	return strcmp(m, mime_uri_list) == 0 || strcmp(m, mime_gnome_files) == 0;
}

static void clip_request_free(void* p)
{
	clip_request* r = p;
	if (r)
		free(r->mime);
	free(r);
}

static void* clip_request_clone(const void* p)
{
	const clip_request* o = p;
	clip_request* r = calloc(1, sizeof(clip_request));
	if (!r)
		return nullptr;
	*r = *o;
	r->mime = o->mime ? _strdup(o->mime) : nullptr;
	return r;
}

static void free_formats(CLIPRDR_FORMAT** formats, size_t* count)
{
	for (size_t i = 0; i < *count; i++)
		free((*formats)[i].formatName);
	free(*formats);
	*formats = nullptr;
	*count = 0;
}

static void add_client_format(ferrum_session* s, UINT32 id)
{
	if (id == 0)
		return;
	for (size_t i = 0; i < s->n_client_formats; i++)
		if (s->client_formats[i].formatId == id)
			return;
	CLIPRDR_FORMAT* f =
	    realloc(s->client_formats, (s->n_client_formats + 1) * sizeof(CLIPRDR_FORMAT));
	if (!f)
		return;
	s->client_formats = f;
	f = &s->client_formats[s->n_client_formats++];
	f->formatId = id;
	f->formatName = nullptr;
	const char* name = ClipboardGetFormatName(s->system, id);
	if (name && id >= CF_MAX)
		f->formatName = _strdup(name);
}

/* call with clip_lock held */
static UINT send_client_format_list(ferrum_session* s)
{
	if (!s->cliprdr || !s->clip_ready)
		return CHANNEL_RC_OK;
	const CLIPRDR_FORMAT_LIST list = { .common.msgType = CB_FORMAT_LIST,
		                               .numFormats = (UINT32)s->n_client_formats,
		                               .formats = s->client_formats };
	cliprdr_file_context_clear(s->file);
	return s->cliprdr->ClientFormatList(s->cliprdr, &list);
}

void ferrum_clip_local_offer(ferrum_session* s, const char* const* mimes, size_t count)
{
	if (!s)
		return;
	EnterCriticalSection(&s->clip_lock);
	free_formats(&s->client_formats, &s->n_client_formats);
	for (size_t i = 0; i < s->n_client_mimes; i++)
		free(s->client_mimes[i]);
	free(s->client_mimes);
	s->client_mimes = nullptr;
	s->n_client_mimes = 0;

	if (count > 0)
		s->client_mimes = calloc(count, sizeof(char*));

	ClipboardLock(s->system);
	for (size_t i = 0; i < count && s->client_mimes; i++)
	{
		const char* m = mimes[i];
		s->client_mimes[s->n_client_mimes++] = _strdup(m);
		if (strcmp(m, mime_html) == 0)
			add_client_format(s, ClipboardGetFormatId(s->system, type_html));
		else if (mime_is_text(m))
		{
			add_client_format(s, CF_UNICODETEXT);
			add_client_format(s, CF_TEXT);
			add_client_format(s, CF_OEMTEXT);
		}
		else if (mime_is_image(m))
		{
			add_client_format(s, CF_DIB);
			add_client_format(s, ClipboardGetFormatId(s->system, mime_png));
		}
		else if (mime_is_file(m))
			add_client_format(s, ClipboardGetFormatId(s->system, type_file_group));
	}
	ClipboardUnlock(s->system);

	send_client_format_list(s);
	LeaveCriticalSection(&s->clip_lock);
}

static BOOL have_client_mime(ferrum_session* s, const char* mime)
{
	for (size_t i = 0; i < s->n_client_mimes; i++)
		if (strcmp(s->client_mimes[i], mime) == 0)
			return TRUE;
	return FALSE;
}

static const char* pick_client_mime(ferrum_session* s, BOOL (*pred)(const char*),
                                    const char* preferred)
{
	if (preferred && have_client_mime(s, preferred))
		return preferred;
	for (size_t i = 0; i < s->n_client_mimes; i++)
		if (pred(s->client_mimes[i]))
			return s->client_mimes[i];
	return nullptr;
}

static UINT send_data_response(ferrum_session* s, const BYTE* data, size_t size)
{
	CLIPRDR_FORMAT_DATA_RESPONSE response = WINPR_C_ARRAY_INIT;
	response.common.msgFlags = data ? CB_RESPONSE_OK : CB_RESPONSE_FAIL;
	response.common.dataLen = data ? (UINT32)size : 0;
	response.requestedFormatData = data;
	return s->cliprdr->ClientFormatDataResponse(s->cliprdr, &response);
}

static UINT clip_monitor_ready(CliprdrClientContext* ctx,
                               WINPR_ATTR_UNUSED const CLIPRDR_MONITOR_READY* ready)
{
	ferrum_session* s = cliprdr_file_context_get_context(ctx->custom);
	CLIPRDR_GENERAL_CAPABILITY_SET general = {
		.capabilitySetType = CB_CAPSTYPE_GENERAL,
		.capabilitySetLength = 12,
		.version = CB_CAPS_VERSION_2,
		.generalFlags = CB_USE_LONG_FORMAT_NAMES | cliprdr_file_context_current_flags(s->file)
	};
	CLIPRDR_CAPABILITIES caps = { .cCapabilitiesSets = 1,
		                          .capabilitySets = (CLIPRDR_CAPABILITY_SET*)&general };
	UINT rc = ctx->ClientCapabilities(ctx, &caps);
	if (rc != CHANNEL_RC_OK)
		return rc;

	EnterCriticalSection(&s->clip_lock);
	s->clip_ready = TRUE;
	rc = send_client_format_list(s);
	LeaveCriticalSection(&s->clip_lock);
	return rc;
}

static UINT clip_server_caps(CliprdrClientContext* ctx, const CLIPRDR_CAPABILITIES* caps)
{
	ferrum_session* s = cliprdr_file_context_get_context(ctx->custom);
	const BYTE* p = (const BYTE*)caps->capabilitySets;
	if (!cliprdr_file_context_remote_set_flags(s->file, 0))
		return ERROR_INTERNAL_ERROR;
	for (UINT32 i = 0; i < caps->cCapabilitiesSets; i++)
	{
		const CLIPRDR_CAPABILITY_SET* c = (const CLIPRDR_CAPABILITY_SET*)p;
		if (c->capabilitySetType == CB_CAPSTYPE_GENERAL)
		{
			const CLIPRDR_GENERAL_CAPABILITY_SET* g = (const CLIPRDR_GENERAL_CAPABILITY_SET*)c;
			if (!cliprdr_file_context_remote_set_flags(s->file, g->generalFlags))
				return ERROR_INTERNAL_ERROR;
		}
		p += c->capabilitySetLength;
	}
	return CHANNEL_RC_OK;
}

static UINT32 server_format_id(ferrum_session* s, const char* name)
{
	for (size_t i = 0; i < s->n_server_formats; i++)
		if (s->server_formats[i].formatName && strcmp(s->server_formats[i].formatName, name) == 0)
			return s->server_formats[i].formatId;
	return 0;
}

static BOOL server_has_format(ferrum_session* s, UINT32 id)
{
	for (size_t i = 0; i < s->n_server_formats; i++)
		if (s->server_formats[i].formatId == id)
			return TRUE;
	return FALSE;
}

static UINT clip_server_format_list(CliprdrClientContext* ctx, const CLIPRDR_FORMAT_LIST* list)
{
	ferrum_session* s = cliprdr_file_context_get_context(ctx->custom);
	BOOL text = FALSE, html = FALSE, image = FALSE, file = FALSE;

	EnterCriticalSection(&s->clip_lock);
	free_formats(&s->server_formats, &s->n_server_formats);
	cliprdr_file_context_clear(s->file);

	s->server_formats = calloc(list->numFormats ? list->numFormats : 1, sizeof(CLIPRDR_FORMAT));
	if (!s->server_formats)
	{
		LeaveCriticalSection(&s->clip_lock);
		return CHANNEL_RC_NO_MEMORY;
	}
	s->n_server_formats = list->numFormats;

	for (UINT32 i = 0; i < list->numFormats; i++)
	{
		const CLIPRDR_FORMAT* f = &list->formats[i];
		s->server_formats[i].formatId = f->formatId;
		s->server_formats[i].formatName = f->formatName ? _strdup(f->formatName) : nullptr;
		if (f->formatName)
		{
			if (strcmp(f->formatName, type_html) == 0)
				html = TRUE;
			else if (strcmp(f->formatName, type_file_group) == 0)
				file = TRUE;
		}
		switch (f->formatId)
		{
			case CF_TEXT:
			case CF_OEMTEXT:
			case CF_UNICODETEXT:
				text = TRUE;
				break;
			case CF_DIB:
			case CF_DIBV5:
				image = TRUE;
				break;
			default:
				break;
		}
	}

	const char* mimes[8];
	size_t n = 0;
	if (file && cliprdr_file_context_has_local_support(s->file))
	{
		mimes[n++] = mime_uri_list;
		mimes[n++] = mime_gnome_files;
	}
	if (html)
		mimes[n++] = mime_html;
	if (text)
		mimes[n++] = mime_text_utf8;
	if (image)
	{
		mimes[n++] = mime_png;
		mimes[n++] = mime_bmp;
	}
	LeaveCriticalSection(&s->clip_lock);

	if (s->cb.clip_remote_offer)
		s->cb.clip_remote_offer(s->cb.user, mimes, n);

	const CLIPRDR_FORMAT_LIST_RESPONSE resp = { .common.msgType = CB_FORMAT_LIST_RESPONSE,
		                                        .common.msgFlags = CB_RESPONSE_OK };
	return ctx->ClientFormatListResponse(ctx, &resp);
}

static UINT clip_server_format_list_response(WINPR_ATTR_UNUSED CliprdrClientContext* ctx,
                                             WINPR_ATTR_UNUSED const CLIPRDR_FORMAT_LIST_RESPONSE* r)
{
	return CHANNEL_RC_OK;
}

/* server asks for our (local) clipboard data */
static UINT clip_server_data_request(CliprdrClientContext* ctx,
                                     const CLIPRDR_FORMAT_DATA_REQUEST* req)
{
	ferrum_session* s = cliprdr_file_context_get_context(ctx->custom);
	const UINT32 format_id = req->requestedFormatId;
	UINT32 local_id = format_id;
	const char* mime = nullptr;
	BYTE* out = nullptr;
	UINT32 out_len = 0;
	BYTE* serialized = nullptr;
	UINT32 serialized_len = 0;

	EnterCriticalSection(&s->clip_lock);
	ClipboardLock(s->system);
	const UINT32 file_id = ClipboardGetFormatId(s->system, type_file_group);
	const UINT32 html_id = ClipboardGetFormatId(s->system, type_html);
	const UINT32 png_id = ClipboardGetFormatId(s->system, mime_png);

	switch (format_id)
	{
		case CF_TEXT:
		case CF_OEMTEXT:
		case CF_UNICODETEXT:
			mime = pick_client_mime(s, mime_is_text, mime_text_utf8);
			local_id = ClipboardGetFormatId(s->system, mime_text_plain);
			break;
		case CF_DIB:
		case CF_DIBV5:
			mime = pick_client_mime(s, mime_is_image, mime_png);
			if (mime)
				local_id = ClipboardGetFormatId(s->system, mime);
			break;
		default:
			if (format_id == file_id)
			{
				mime = pick_client_mime(s, mime_is_file, mime_uri_list);
				local_id = ClipboardGetFormatId(s->system, mime_uri_list);
			}
			else if (format_id == html_id)
			{
				mime = have_client_mime(s, mime_html) ? mime_html : nullptr;
				local_id = ClipboardGetFormatId(s->system, mime_html);
			}
			else if (format_id == png_id)
			{
				mime = pick_client_mime(s, mime_is_image, mime_png);
				if (mime)
					local_id = ClipboardGetFormatId(s->system, mime);
			}
			break;
	}
	char* mime_copy = mime ? _strdup(mime) : nullptr;
	ClipboardUnlock(s->system);
	LeaveCriticalSection(&s->clip_lock);

	if (!mime_copy || !s->cb.clip_local_get)
	{
		free(mime_copy);
		return send_data_response(s, nullptr, 0);
	}

	/* may block while the UI thread reads the local clipboard */
	size_t len = 0;
	uint8_t* data = s->cb.clip_local_get(s->cb.user, mime_copy, &len);

	EnterCriticalSection(&s->clip_lock);
	ClipboardLock(s->system);
	if (data && len > 0 && len < UINT32_MAX)
	{
		/* the gnome format has a leading "copy\n" line, normalize to uri-list */
		if (strcmp(mime_copy, mime_gnome_files) == 0)
		{
			char* nl = memchr(data, '\n', len);
			if (nl)
			{
				size_t skip = (size_t)((uint8_t*)nl - data) + 1;
				memmove(data, data + skip, len - skip);
				len -= skip;
			}
		}

		BOOL ok = TRUE;
		if (format_id == file_id)
			ok = cliprdr_file_context_update_client_data(s->file, (const char*)data, len);

		if (ok && ClipboardSetData(s->system, local_id, data, (UINT32)len))
			out = ClipboardGetData(s->system, format_id, &out_len);

		if (out && format_id == file_id)
		{
			const UINT32 flags = cliprdr_file_context_remote_get_flags(s->file);
			const UINT32 err =
			    cliprdr_serialize_file_list_ex(flags, (const FILEDESCRIPTORW*)out,
			                                   out_len / sizeof(FILEDESCRIPTORW), &serialized,
			                                   &serialized_len);
			free(out);
			out = err ? nullptr : serialized;
			out_len = err ? 0 : serialized_len;
		}
	}
	ClipboardUnlock(s->system);
	LeaveCriticalSection(&s->clip_lock);

	free(data);
	free(mime_copy);
	const UINT rc = send_data_response(s, out, out_len);
	free(out);
	return rc;
}

void ferrum_clip_request_remote(ferrum_session* s, const char* mime, uint64_t request_id)
{
	if (!s || !mime)
		return;

	EnterCriticalSection(&s->clip_lock);
	UINT32 format_id = 0;
	if (s->cliprdr)
	{
		if (mime_is_file(mime))
			format_id = server_format_id(s, type_file_group);
		else if (strcmp(mime, mime_html) == 0)
			format_id = server_format_id(s, type_html);
		else if (mime_is_text(mime))
			format_id = CF_UNICODETEXT;
		else if (mime_is_image(mime))
			format_id = server_has_format(s, CF_DIBV5) && !server_has_format(s, CF_DIB)
			                ? CF_DIBV5
			                : CF_DIB;
	}

	if (format_id == 0)
	{
		LeaveCriticalSection(&s->clip_lock);
		if (s->cb.clip_remote_data)
			s->cb.clip_remote_data(s->cb.user, request_id, nullptr, 0);
		return;
	}

	clip_request rq = { .mime = (char*)mime, .format_id = format_id, .request_id = request_id };
	Queue_Enqueue(s->requests, &rq);
	CLIPRDR_FORMAT_DATA_REQUEST req = { .requestedFormatId = format_id };
	CliprdrClientContext* ctx = s->cliprdr;
	LeaveCriticalSection(&s->clip_lock);
	ctx->ClientFormatDataRequest(ctx, &req);
}

/* server answers our request for remote clipboard data */
static UINT clip_server_data_response(CliprdrClientContext* ctx,
                                      const CLIPRDR_FORMAT_DATA_RESPONSE* resp)
{
	ferrum_session* s = cliprdr_file_context_get_context(ctx->custom);
	clip_request* rq = Queue_Dequeue(s->requests);
	if (!rq)
		return CHANNEL_RC_OK;

	BYTE* out = nullptr;
	UINT32 out_len = 0;

	if (!(resp->common.msgFlags & CB_RESPONSE_FAIL))
	{
		EnterCriticalSection(&s->clip_lock);
		ClipboardLock(s->system);
		const UINT32 file_id = ClipboardGetFormatId(s->system, type_file_group);
		const UINT32 html_id = ClipboardGetFormatId(s->system, type_html);
		UINT32 src = 0;
		UINT32 dst = 0;
		const char* dst_mime = rq->mime;

		if (rq->format_id == server_format_id(s, type_file_group))
		{
			src = file_id;
			dst = ClipboardGetFormatId(s->system, mime_uri_list);
			if (!cliprdr_file_context_update_server_data(s->file, s->system,
			                                             resp->requestedFormatData,
			                                             resp->common.dataLen))
				src = 0;
		}
		else if (rq->format_id == server_format_id(s, type_html))
		{
			src = html_id;
			dst = ClipboardGetFormatId(s->system, mime_html);
		}
		else if (rq->format_id == CF_UNICODETEXT)
		{
			src = CF_UNICODETEXT;
			dst = ClipboardGetFormatId(s->system, mime_text_plain);
		}
		else
		{
			src = rq->format_id;
			dst = ClipboardGetFormatId(s->system, dst_mime);
		}

		if (src && dst &&
		    ClipboardSetData(s->system, src, resp->requestedFormatData, resp->common.dataLen))
			out = ClipboardGetData(s->system, dst, &out_len);

		ClipboardUnlock(s->system);
		LeaveCriticalSection(&s->clip_lock);

		/* text conversions include a trailing NUL */
		if (out && dst_mime && mime_is_text(dst_mime))
			out_len = (UINT32)strnlen((const char*)out, out_len);
		/* the gnome format needs a "copy" verb line */
		if (out && strcmp(rq->mime, mime_gnome_files) == 0)
		{
			BYTE* g = malloc(out_len + 6);
			if (g)
			{
				memcpy(g, "copy\n", 5);
				memcpy(g + 5, out, out_len);
				free(out);
				out = g;
				out_len += 5;
			}
		}
	}

	if (s->cb.clip_remote_data)
		s->cb.clip_remote_data(s->cb.user, rq->request_id, out, out ? out_len : 0);
	free(out);
	clip_request_free(rq);
	return CHANNEL_RC_OK;
}

static void clip_init(ferrum_session* s, CliprdrClientContext* cliprdr)
{
	EnterCriticalSection(&s->clip_lock);
	s->cliprdr = cliprdr;
	s->clip_ready = FALSE;
	cliprdr->MonitorReady = clip_monitor_ready;
	cliprdr->ServerCapabilities = clip_server_caps;
	cliprdr->ServerFormatList = clip_server_format_list;
	cliprdr->ServerFormatListResponse = clip_server_format_list_response;
	cliprdr->ServerFormatDataRequest = clip_server_data_request;
	cliprdr->ServerFormatDataResponse = clip_server_data_response;
	cliprdr_file_context_init(s->file, cliprdr);
	LeaveCriticalSection(&s->clip_lock);
}

static void clip_uninit(ferrum_session* s, CliprdrClientContext* cliprdr)
{
	EnterCriticalSection(&s->clip_lock);
	cliprdr_file_context_uninit(s->file, cliprdr);
	cliprdr->custom = nullptr;
	s->cliprdr = nullptr;
	s->clip_ready = FALSE;
	LeaveCriticalSection(&s->clip_lock);
}

/* ------------------------------------------------------------------ */
/* RemoteApp                                                          */
/* ------------------------------------------------------------------ */

static BOOL rail_window_common(rdpContext* context, const WINDOW_ORDER_INFO* info,
                               const WINDOW_STATE_ORDER* state)
{
	ferrum_session* s = get_session(context);
	if (!s || !s->cb.rail_window)
		return TRUE;

	const UINT32 f = info->fieldFlags;
	ferrum_rail_window w = { 0 };
	char* title = nullptr;
	w.window_id = info->windowId;

	if (f & WINDOW_ORDER_FIELD_TITLE)
	{
		title = rail_string_to_utf8_string(&state->titleInfo);
		w.title = title;
		w.fields |= FERRUM_RAIL_TITLE;
	}
	if (f & WINDOW_ORDER_STATE_NEW)
		w.fields |= FERRUM_RAIL_NEW;
	if (f & WINDOW_ORDER_FIELD_WND_OFFSET)
	{
		w.x = state->windowOffsetX;
		w.y = state->windowOffsetY;
		w.fields |= FERRUM_RAIL_OFFSET;
	}
	if (f & WINDOW_ORDER_FIELD_WND_SIZE)
	{
		w.width = (int32_t)state->windowWidth;
		w.height = (int32_t)state->windowHeight;
		w.fields |= FERRUM_RAIL_SIZE;
	}
	if (f & WINDOW_ORDER_FIELD_SHOW)
	{
		w.show_state = state->showState;
		w.fields |= FERRUM_RAIL_STATE;
	}
	if (f & WINDOW_ORDER_FIELD_STYLE)
	{
		w.style = state->style;
		w.ex_style = state->extendedStyle;
		w.fields |= FERRUM_RAIL_STYLE;
	}
	if (f & WINDOW_ORDER_FIELD_OWNER)
	{
		w.owner_id = state->ownerWindowId;
		w.fields |= FERRUM_RAIL_OWNER;
	}
	s->cb.rail_window(s->cb.user, &w);
	free(title);
	return TRUE;
}

static BOOL rail_window_delete(rdpContext* context, const WINDOW_ORDER_INFO* info)
{
	ferrum_session* s = get_session(context);
	if (s && s->cb.rail_window_delete)
		s->cb.rail_window_delete(s->cb.user, info->windowId);
	return TRUE;
}

static void icon_free(void* p)
{
	ferrum_icon* i = p;
	if (i)
		free(i->bgra);
	free(i);
}

static void emit_icon(ferrum_session* s, UINT64 window_id, const ferrum_icon* icon)
{
	if (icon && s->cb.rail_window_icon)
		s->cb.rail_window_icon(s->cb.user, window_id, icon->bgra, icon->width, icon->height);
}

static BOOL rail_window_icon(rdpContext* context, const WINDOW_ORDER_INFO* info,
                             const WINDOW_ICON_ORDER* order)
{
	ferrum_session* s = get_session(context);
	const ICON_INFO* ii = order->iconInfo;
	if (!s || !ii || ii->width == 0 || ii->height == 0 || ii->width > 512 || ii->height > 512)
		return TRUE;

	ferrum_icon* icon = calloc(1, sizeof(ferrum_icon));
	if (!icon)
		return TRUE;
	icon->width = ii->width;
	icon->height = ii->height;
	icon->bgra = calloc(4ull * ii->width * ii->height, 1);
	if (!icon->bgra ||
	    !freerdp_image_copy_from_icon_data(icon->bgra, PIXEL_FORMAT_BGRA32, 0, 0, 0,
	                                       (UINT16)ii->width, (UINT16)ii->height, ii->bitsColor,
	                                       (UINT16)ii->cbBitsColor, ii->bitsMask,
	                                       (UINT16)ii->cbBitsMask, ii->colorTable,
	                                       (UINT16)ii->cbColorTable, ii->bpp))
	{
		icon_free(icon);
		return TRUE;
	}

	emit_icon(s, info->windowId, icon);
	if (ii->cacheEntry != 0xFFFF && s->icons)
	{
		const UINT64 key = ((UINT64)ii->cacheId << 16) | ii->cacheEntry;
		HashTable_Remove(s->icons, (void*)(uintptr_t)(key + 1));
		if (!HashTable_Insert(s->icons, (void*)(uintptr_t)(key + 1), icon))
			icon_free(icon);
	}
	else
		icon_free(icon);
	return TRUE;
}

static BOOL rail_window_cached_icon(rdpContext* context, const WINDOW_ORDER_INFO* info,
                                    const WINDOW_CACHED_ICON_ORDER* order)
{
	ferrum_session* s = get_session(context);
	if (!s || !s->icons)
		return TRUE;
	const UINT64 key =
	    ((UINT64)order->cachedIcon.cacheId << 16) | order->cachedIcon.cacheEntry;
	emit_icon(s, info->windowId, HashTable_GetItemValue(s->icons, (void*)(uintptr_t)(key + 1)));
	return TRUE;
}

static BOOL rail_noop_notify(WINPR_ATTR_UNUSED rdpContext* context,
                             WINPR_ATTR_UNUSED const WINDOW_ORDER_INFO* info,
                             WINPR_ATTR_UNUSED const NOTIFY_ICON_STATE_ORDER* state)
{
	return TRUE;
}

static BOOL rail_noop_notify_delete(WINPR_ATTR_UNUSED rdpContext* context,
                                    WINPR_ATTR_UNUSED const WINDOW_ORDER_INFO* info)
{
	return TRUE;
}

static BOOL rail_monitored_desktop(WINPR_ATTR_UNUSED rdpContext* context,
                                   WINPR_ATTR_UNUSED const WINDOW_ORDER_INFO* info,
                                   WINPR_ATTR_UNUSED const MONITORED_DESKTOP_ORDER* order)
{
	return TRUE;
}

static BOOL rail_non_monitored_desktop(WINPR_ATTR_UNUSED rdpContext* context,
                                       WINPR_ATTR_UNUSED const WINDOW_ORDER_INFO* info)
{
	return TRUE;
}

static UINT rail_handshake(RailClientContext* rail,
                           WINPR_ATTR_UNUSED const RAIL_HANDSHAKE_ORDER* handshake)
{
	return client_rail_server_start_cmd(rail);
}

static UINT rail_handshake_ex(RailClientContext* rail,
                              WINPR_ATTR_UNUSED const RAIL_HANDSHAKE_EX_ORDER* handshake)
{
	return client_rail_server_start_cmd(rail);
}

static UINT rail_exec_result(RailClientContext* rail, const RAIL_EXEC_RESULT_ORDER* result)
{
	ferrum_session* s = rail->custom;
	if (s && s->cb.rail_exec_result)
	{
		char* exe = rail_string_to_utf8_string(&result->exeOrFile);
		s->cb.rail_exec_result(s->cb.user, result->execResult, result->rawResult, exe);
		free(exe);
	}
	return CHANNEL_RC_OK;
}

static UINT rail_noop_sysparam(WINPR_ATTR_UNUSED RailClientContext* rail,
                               WINPR_ATTR_UNUSED const RAIL_SYSPARAM_ORDER* p)
{
	return CHANNEL_RC_OK;
}

static UINT rail_noop_movesize(WINPR_ATTR_UNUSED RailClientContext* rail,
                               WINPR_ATTR_UNUSED const RAIL_LOCALMOVESIZE_ORDER* p)
{
	return CHANNEL_RC_OK;
}

static UINT rail_noop_minmax(WINPR_ATTR_UNUSED RailClientContext* rail,
                             WINPR_ATTR_UNUSED const RAIL_MINMAXINFO_ORDER* p)
{
	return CHANNEL_RC_OK;
}

static UINT rail_noop_langbar(WINPR_ATTR_UNUSED RailClientContext* rail,
                              WINPR_ATTR_UNUSED const RAIL_LANGBAR_INFO_ORDER* p)
{
	return CHANNEL_RC_OK;
}

static UINT rail_noop_appid(WINPR_ATTR_UNUSED RailClientContext* rail,
                            WINPR_ATTR_UNUSED const RAIL_GET_APPID_RESP_ORDER* p)
{
	return CHANNEL_RC_OK;
}

static void rail_init(ferrum_session* s, RailClientContext* rail)
{
	rdpWindowUpdate* window = s->context->update->window;
	window->WindowCreate = rail_window_common;
	window->WindowUpdate = rail_window_common;
	window->WindowDelete = rail_window_delete;
	window->WindowIcon = rail_window_icon;
	window->WindowCachedIcon = rail_window_cached_icon;
	window->NotifyIconCreate = rail_noop_notify;
	window->NotifyIconUpdate = rail_noop_notify;
	window->NotifyIconDelete = rail_noop_notify_delete;
	window->MonitoredDesktop = rail_monitored_desktop;
	window->NonMonitoredDesktop = rail_non_monitored_desktop;

	rail->custom = s;
	rail->ServerHandshake = rail_handshake;
	rail->ServerHandshakeEx = rail_handshake_ex;
	rail->ServerExecuteResult = rail_exec_result;
	rail->ServerSystemParam = rail_noop_sysparam;
	rail->ServerLocalMoveSize = rail_noop_movesize;
	rail->ServerMinMaxInfo = rail_noop_minmax;
	rail->ServerLanguageBarInfo = rail_noop_langbar;
	rail->ServerGetAppIdResponse = rail_noop_appid;

	EnterCriticalSection(&s->lock);
	s->rail = rail;
	LeaveCriticalSection(&s->lock);
}

void ferrum_rail_activate(ferrum_session* s, uint64_t window_id, bool active)
{
	if (!s)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active && s->rail)
	{
		RAIL_ACTIVATE_ORDER a = { .windowId = (UINT32)window_id, .enabled = active };
		s->rail->ClientActivate(s->rail, &a);
	}
	LeaveCriticalSection(&s->lock);
}

void ferrum_rail_syscommand(ferrum_session* s, uint64_t window_id, uint16_t command)
{
	if (!s)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active && s->rail)
	{
		RAIL_SYSCOMMAND_ORDER c = { .windowId = (UINT32)window_id, .command = command };
		s->rail->ClientSystemCommand(s->rail, &c);
	}
	LeaveCriticalSection(&s->lock);
}

void ferrum_rail_move(ferrum_session* s, uint64_t window_id, int32_t left, int32_t top,
                      int32_t right, int32_t bottom)
{
	if (!s)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active && s->rail)
	{
		RAIL_WINDOW_MOVE_ORDER m = { .windowId = (UINT32)window_id,
			                         .left = (INT16)left,
			                         .top = (INT16)top,
			                         .right = (INT16)right,
			                         .bottom = (INT16)bottom };
		s->rail->ClientWindowMove(s->rail, &m);
	}
	LeaveCriticalSection(&s->lock);
}

void ferrum_rail_exec(ferrum_session* s, const char* program, const char* workdir,
                      const char* args)
{
	if (!s || !program)
		return;
	EnterCriticalSection(&s->lock);
	if (s->active && s->rail)
	{
		RAIL_EXEC_ORDER e = { .RemoteApplicationProgram = program,
			                  .RemoteApplicationWorkingDir = workdir,
			                  .RemoteApplicationArguments = args };
		s->rail->ClientExecute(s->rail, &e);
	}
	LeaveCriticalSection(&s->lock);
}

/* ------------------------------------------------------------------ */
/* channels                                                           */
/* ------------------------------------------------------------------ */

static void on_channel_connected(void* context, const ChannelConnectedEventArgs* e)
{
	ferrum_session* s = get_session(context);
	if (strcmp(e->name, CLIPRDR_SVC_CHANNEL_NAME) == 0)
		clip_init(s, (CliprdrClientContext*)e->pInterface);
	else if (strcmp(e->name, DISP_DVC_CHANNEL_NAME) == 0)
	{
		DispClientContext* disp = e->pInterface;
		disp->custom = s;
		disp->DisplayControlCaps = ferrum_disp_caps;
		EnterCriticalSection(&s->lock);
		s->disp = disp;
		LeaveCriticalSection(&s->lock);
	}
	else if (strcmp(e->name, RAIL_SVC_CHANNEL_NAME) == 0)
		rail_init(s, (RailClientContext*)e->pInterface);
	else
		freerdp_client_OnChannelConnectedEventHandler(context, e);
}

static void on_channel_disconnected(void* context, const ChannelDisconnectedEventArgs* e)
{
	ferrum_session* s = get_session(context);
	if (strcmp(e->name, CLIPRDR_SVC_CHANNEL_NAME) == 0)
		clip_uninit(s, (CliprdrClientContext*)e->pInterface);
	else if (strcmp(e->name, DISP_DVC_CHANNEL_NAME) == 0)
	{
		EnterCriticalSection(&s->lock);
		s->disp = nullptr;
		s->disp_activated = FALSE;
		LeaveCriticalSection(&s->lock);
	}
	else if (strcmp(e->name, RAIL_SVC_CHANNEL_NAME) == 0)
	{
		EnterCriticalSection(&s->lock);
		s->rail = nullptr;
		LeaveCriticalSection(&s->lock);
	}
	else
		freerdp_client_OnChannelDisconnectedEventHandler(context, e);
}

/* ------------------------------------------------------------------ */
/* connection callbacks                                               */
/* ------------------------------------------------------------------ */

static BOOL ferrum_pre_connect(freerdp* instance)
{
	rdpContext* context = instance->context;
	rdpSettings* settings = context->settings;

	if (!freerdp_settings_set_uint32(settings, FreeRDP_OsMajorType, OSMAJORTYPE_UNIX) ||
	    !freerdp_settings_set_uint32(settings, FreeRDP_OsMinorType, OSMINORTYPE_NATIVE_WAYLAND))
		return FALSE;

	if (PubSub_SubscribeChannelConnected(context->pubSub, on_channel_connected) < 0 ||
	    PubSub_SubscribeChannelDisconnected(context->pubSub, on_channel_disconnected) < 0)
		return FALSE;

	return TRUE;
}

static BOOL ferrum_post_connect(freerdp* instance)
{
	rdpContext* context = instance->context;
	ferrum_session* s = get_session(context);

	if (!gdi_init(instance, PIXEL_FORMAT_BGRX32))
		return FALSE;

	register_pointer(context->graphics);
	context->update->EndPaint = ferrum_end_paint;
	context->update->DesktopResize = ferrum_desktop_resize;

	EnterCriticalSection(&s->lock);
	s->active = TRUE;
	LeaveCriticalSection(&s->lock);

	rdpGdi* gdi = context->gdi;
	if (s->cb.on_desktop_resize)
		s->cb.on_desktop_resize(s->cb.user, (uint32_t)gdi->width, (uint32_t)gdi->height);
	if (s->cb.on_state)
		s->cb.on_state(s->cb.user, FERRUM_STATE_CONNECTED);
	emit_full_frame(s);
	return TRUE;
}

static void ferrum_post_disconnect(freerdp* instance)
{
	rdpContext* context = instance->context;
	ferrum_session* s = get_session(context);
	if (s)
	{
		EnterCriticalSection(&s->lock);
		s->active = FALSE;
		LeaveCriticalSection(&s->lock);
	}
	PubSub_UnsubscribeChannelConnected(context->pubSub, on_channel_connected);
	PubSub_UnsubscribeChannelDisconnected(context->pubSub, on_channel_disconnected);
	gdi_free(instance);
}

static BOOL ferrum_authenticate(freerdp* instance, char** username, char** password,
                                char** domain, rdp_auth_reason reason)
{
	ferrum_session* s = get_session(instance->context);
	if (!s || !s->cb.authenticate)
		return FALSE;
	return s->cb.authenticate(s->cb.user, (int)reason, username, password, domain);
}

static DWORD ferrum_verify_certificate(freerdp* instance, const char* host, UINT16 port,
                                       const char* common_name, const char* subject,
                                       const char* issuer, const char* fingerprint, DWORD flags)
{
	ferrum_session* s = get_session(instance->context);
	if (!s || !s->cb.verify_certificate)
		return 0;
	const ferrum_cert_info info = { .host = host,
		                            .port = port,
		                            .common_name = common_name,
		                            .subject = subject,
		                            .issuer = issuer,
		                            .fingerprint = fingerprint,
		                            .flags = flags };
	return (DWORD)s->cb.verify_certificate(s->cb.user, &info);
}

static DWORD ferrum_verify_changed_certificate(freerdp* instance, const char* host, UINT16 port,
                                               const char* common_name, const char* subject,
                                               const char* issuer, const char* fingerprint,
                                               const char* old_subject, const char* old_issuer,
                                               const char* old_fingerprint, DWORD flags)
{
	ferrum_session* s = get_session(instance->context);
	if (!s || !s->cb.verify_certificate)
		return 0;
	const ferrum_cert_info info = { .host = host,
		                            .port = port,
		                            .common_name = common_name,
		                            .subject = subject,
		                            .issuer = issuer,
		                            .fingerprint = fingerprint,
		                            .old_subject = old_subject,
		                            .old_issuer = old_issuer,
		                            .old_fingerprint = old_fingerprint,
		                            .flags = flags | VERIFY_CERT_FLAG_CHANGED };
	return (DWORD)s->cb.verify_certificate(s->cb.user, &info);
}

static BOOL ferrum_gateway_message(freerdp* instance, UINT32 type, BOOL display_mandatory,
                                   BOOL consent_mandatory, size_t length, const WCHAR* message)
{
	ferrum_session* s = get_session(instance->context);
	if (!s || !s->cb.gateway_message)
		return !consent_mandatory;
	char* text = ConvertWCharNToUtf8Alloc(message, length / sizeof(WCHAR), nullptr);
	const BOOL rc = s->cb.gateway_message(s->cb.user, type, display_mandatory, consent_mandatory,
	                                      text ? text : "");
	free(text);
	return rc;
}

static BOOL ferrum_choose_smartcard(freerdp* instance, SmartcardCertInfo** cert_list, DWORD count,
                                    DWORD* choice, WINPR_ATTR_UNUSED BOOL gateway)
{
	ferrum_session* s = get_session(instance->context);
	if (count == 0)
		return FALSE;
	if (count == 1 || !s || !s->cb.choose_smartcard)
	{
		*choice = 0;
		return TRUE;
	}

	char** names = calloc(count, sizeof(char*));
	if (!names)
		return FALSE;
	for (DWORD i = 0; i < count; i++)
	{
		const SmartcardCertInfo* c = cert_list[i];
		char* reader = ConvertWCharToUtf8Alloc(c->reader, nullptr);
		size_t len = 0;
		winpr_asprintf(&names[i], &len, "%s — %s (%s)", c->userHint ? c->userHint : "",
		               c->subject ? c->subject : "", reader ? reader : "");
		free(reader);
	}
	const int idx = s->cb.choose_smartcard(s->cb.user, (const char* const*)names, count);
	for (DWORD i = 0; i < count; i++)
		free(names[i]);
	free(names);
	if (idx < 0 || (DWORD)idx >= count)
		return FALSE;
	*choice = (DWORD)idx;
	return TRUE;
}

static int ferrum_logon_error_info(freerdp* instance, UINT32 data, UINT32 type)
{
	ferrum_session* s = get_session(instance->context);
	if (s && s->cb.logon_error)
		s->cb.logon_error(s->cb.user, freerdp_get_logon_error_info_data(data),
		                  freerdp_get_logon_error_info_type(type));
	return 1;
}

static BOOL ferrum_client_new(freerdp* instance, WINPR_ATTR_UNUSED rdpContext* context)
{
	instance->PreConnect = ferrum_pre_connect;
	instance->PostConnect = ferrum_post_connect;
	instance->PostDisconnect = ferrum_post_disconnect;
	instance->AuthenticateEx = ferrum_authenticate;
	instance->VerifyCertificateEx = ferrum_verify_certificate;
	instance->VerifyChangedCertificateEx = ferrum_verify_changed_certificate;
	instance->PresentGatewayMessage = ferrum_gateway_message;
	instance->ChooseSmartcard = ferrum_choose_smartcard;
	instance->LogonErrorInfo = ferrum_logon_error_info;
	return TRUE;
}

static void ferrum_client_free(WINPR_ATTR_UNUSED freerdp* instance,
                               WINPR_ATTR_UNUSED rdpContext* context)
{
}

/* ------------------------------------------------------------------ */
/* lifecycle                                                          */
/* ------------------------------------------------------------------ */

ferrum_session* ferrum_session_new(const ferrum_callbacks* callbacks)
{
	ferrum_session* s = calloc(1, sizeof(ferrum_session));
	if (!s)
		return nullptr;
	if (callbacks)
		s->cb = *callbacks;
	InitializeCriticalSection(&s->lock);
	InitializeCriticalSection(&s->clip_lock);

	RDP_CLIENT_ENTRY_POINTS ep = WINPR_C_ARRAY_INIT;
	ep.Version = RDP_CLIENT_INTERFACE_VERSION;
	ep.Size = sizeof(RDP_CLIENT_ENTRY_POINTS_V1);
	ep.ContextSize = sizeof(ferrum_context);
	ep.ClientNew = ferrum_client_new;
	ep.ClientFree = ferrum_client_free;

	s->context = freerdp_client_context_new(&ep);
	if (!s->context)
		goto fail;
	((ferrum_context*)s->context)->s = s;

	s->system = ClipboardCreate();
	s->file = cliprdr_file_context_new(s);
	s->requests = Queue_New(TRUE, -1, -1);
	s->icons = HashTable_New(TRUE);
	if (!s->system || !s->file || !s->requests || !s->icons)
		goto fail;
	cliprdr_file_context_set_locally_available(s->file, TRUE);
	{
		wObject* obj = Queue_Object(s->requests);
		obj->fnObjectNew = clip_request_clone;
		obj->fnObjectFree = clip_request_free;
		obj = HashTable_ValueObject(s->icons);
		obj->fnObjectFree = icon_free;
	}

	/* sane GUI defaults; parse_args may override */
	rdpSettings* settings = s->context->settings;
	freerdp_settings_set_bool(settings, FreeRDP_CertificateCallbackPreferPEM, FALSE);
	freerdp_settings_set_bool(settings, FreeRDP_UseCommonStdioCallbacks, FALSE);
	return s;

fail:
	ferrum_session_free(s);
	return nullptr;
}

void ferrum_session_free(ferrum_session* s)
{
	if (!s)
		return;
	if (s->context)
		freerdp_client_context_free(s->context);
	cliprdr_file_context_free(s->file);
	ClipboardDestroy(s->system);
	Queue_Free(s->requests);
	HashTable_Free(s->icons);
	free_formats(&s->server_formats, &s->n_server_formats);
	free_formats(&s->client_formats, &s->n_client_formats);
	for (size_t i = 0; i < s->n_client_mimes; i++)
		free(s->client_mimes[i]);
	free(s->client_mimes);
	DeleteCriticalSection(&s->clip_lock);
	DeleteCriticalSection(&s->lock);
	free(s);
}

int ferrum_session_parse_args(ferrum_session* s, int argc, const char* const* argv)
{
	if (!s || argc < 1)
		return -1;
	/* FreeRDP may rewrite argv entries in place (e.g. to blank out passwords) */
	char** copy = calloc((size_t)argc + 1, sizeof(char*));
	if (!copy)
		return -1;
	for (int i = 0; i < argc; i++)
		copy[i] = _strdup(argv[i]);
	const int rc = freerdp_client_settings_parse_command_line(s->context->settings, argc, copy,
	                                                          FALSE);
	for (int i = 0; i < argc; i++)
		free(copy[i]);
	free(copy);
	/* never fall back to terminal prompts */
	freerdp_settings_set_bool(s->context->settings, FreeRDP_UseCommonStdioCallbacks, FALSE);
	return rc;
}

bool ferrum_session_set(ferrum_session* s, const char* name, const char* value)
{
	if (!s || !name)
		return false;
	return freerdp_settings_set_value_for_name(s->context->settings, name, value);
}

bool ferrum_session_set_monitors(ferrum_session* s, const ferrum_monitor* monitors, size_t count)
{
	if (!s || !monitors || count == 0 || count > 16)
		return false;
	rdpSettings* settings = s->context->settings;
	if (!freerdp_settings_set_pointer_len(settings, FreeRDP_MonitorDefArray, nullptr, count))
		return false;

	/* RDP wants the primary monitor at origin 0,0 */
	int32_t ox = 0, oy = 0;
	for (size_t i = 0; i < count; i++)
		if (monitors[i].primary)
		{
			ox = monitors[i].x;
			oy = monitors[i].y;
		}

	INT32 minx = INT32_MAX, miny = INT32_MAX, maxx = INT32_MIN, maxy = INT32_MIN;
	for (size_t i = 0; i < count; i++)
	{
		const ferrum_monitor* m = &monitors[i];
		rdpMonitor* d = freerdp_settings_get_pointer_array_writable(settings,
		                                                            FreeRDP_MonitorDefArray, i);
		if (!d)
			return false;
		d->x = m->x - ox;
		d->y = m->y - oy;
		d->width = m->width;
		d->height = m->height;
		d->is_primary = m->primary;
		d->orig_screen = m->id;
		d->attributes.physicalWidth = m->physical_width_mm;
		d->attributes.physicalHeight = m->physical_height_mm;
		d->attributes.orientation = m->orientation;
		d->attributes.desktopScaleFactor = m->desktop_scale ? m->desktop_scale : 100;
		d->attributes.deviceScaleFactor = m->device_scale ? m->device_scale : 100;
		minx = MIN(minx, m->x);
		miny = MIN(miny, m->y);
		maxx = MAX(maxx, m->x + m->width);
		maxy = MAX(maxy, m->y + m->height);
	}
	return freerdp_settings_set_uint32(settings, FreeRDP_MonitorCount, (UINT32)count) &&
	       freerdp_settings_set_bool(settings, FreeRDP_UseMultimon, TRUE) &&
	       freerdp_settings_set_bool(settings, FreeRDP_SupportMonitorLayoutPdu, TRUE) &&
	       freerdp_settings_set_uint32(settings, FreeRDP_DesktopWidth, (UINT32)(maxx - minx)) &&
	       freerdp_settings_set_uint32(settings, FreeRDP_DesktopHeight, (UINT32)(maxy - miny)) &&
	       freerdp_settings_set_int32(settings, FreeRDP_MonitorLocalShiftX, minx - ox) &&
	       freerdp_settings_set_int32(settings, FreeRDP_MonitorLocalShiftY, miny - oy);
}

static BOOL reconnect_window_events(freerdp* instance)
{
	return !freerdp_shall_disconnect_context(instance->context);
}

uint32_t ferrum_session_run(ferrum_session* s)
{
	if (!s)
		return (uint32_t)-1;

	rdpContext* context = s->context;
	freerdp* instance = context->instance;

	if (freerdp_client_start(context) != 0)
		return FREERDP_ERROR_CONNECT_FAILED;

	if (s->cb.on_state)
		s->cb.on_state(s->cb.user, FERRUM_STATE_CONNECTING);

	if (!freerdp_connect(instance))
	{
		UINT32 err = freerdp_get_last_error(context);
		if (err == FREERDP_ERROR_SUCCESS)
			err = FREERDP_ERROR_CONNECT_FAILED;
		freerdp_client_stop(context);
		if (s->cb.on_state)
			s->cb.on_state(s->cb.user, FERRUM_STATE_DISCONNECTED);
		return err;
	}

	while (!freerdp_shall_disconnect_context(context))
	{
		HANDLE handles[MAXIMUM_WAIT_OBJECTS] = WINPR_C_ARRAY_INIT;
		const DWORD count = freerdp_get_event_handles(context, handles, ARRAYSIZE(handles));
		if (count == 0)
			break;

		if (WaitForMultipleObjects(count, handles, FALSE, 100) == WAIT_FAILED)
			break;

		if (!freerdp_check_event_handles(context))
		{
			if (freerdp_shall_disconnect_context(context))
				break;
			if (s->cb.on_state)
				s->cb.on_state(s->cb.user, FERRUM_STATE_RECONNECTING);
			EnterCriticalSection(&s->lock);
			s->active = FALSE;
			LeaveCriticalSection(&s->lock);
			if (client_auto_reconnect_ex(instance, reconnect_window_events))
				continue;
			break;
		}
	}

	UINT32 err = freerdp_get_last_error(context);
	freerdp_disconnect(instance);
	freerdp_client_stop(context);
	if (s->cb.on_state)
		s->cb.on_state(s->cb.user, FERRUM_STATE_DISCONNECTED);
	return err;
}

void ferrum_session_stop(ferrum_session* s)
{
	if (s && s->context)
		freerdp_abort_connect_context(s->context);
}

/* ------------------------------------------------------------------ */
/* input                                                              */
/* ------------------------------------------------------------------ */

#define WITH_ACTIVE(s, body)                     \
	do                                           \
	{                                            \
		if (!(s))                                \
			return;                              \
		EnterCriticalSection(&(s)->lock);        \
		if ((s)->active)                         \
		{                                        \
			body;                                \
		}                                        \
		LeaveCriticalSection(&(s)->lock);        \
	} while (0)

void ferrum_input_key(ferrum_session* s, bool down, bool repeat, uint32_t scancode)
{
	/* Pause has no break code; FreeRDP synthesizes the Ctrl+NumLock sequence */
	if (scancode == RDP_SCANCODE_PAUSE)
	{
		if (down && !repeat)
			WITH_ACTIVE(s, freerdp_input_send_keyboard_pause_event(s->context->input));
		return;
	}
	WITH_ACTIVE(s, freerdp_input_send_keyboard_event_ex(s->context->input, down, repeat,
	                                                    scancode));
}

void ferrum_input_unicode(ferrum_session* s, bool down, uint16_t codepoint)
{
	WITH_ACTIVE(s, freerdp_input_send_unicode_keyboard_event(s->context->input,
	                                                        down ? 0 : KBD_FLAGS_RELEASE,
	                                                        codepoint));
}

void ferrum_input_sync_locks(ferrum_session* s, bool scroll, bool num, bool caps, bool kana)
{
	UINT32 flags = (scroll ? KBD_SYNC_SCROLL_LOCK : 0) | (num ? KBD_SYNC_NUM_LOCK : 0) |
	               (caps ? KBD_SYNC_CAPS_LOCK : 0) | (kana ? KBD_SYNC_KANA_LOCK : 0);
	WITH_ACTIVE(s, freerdp_input_send_synchronize_event(s->context->input, flags));
}

void ferrum_input_focus_in(ferrum_session* s)
{
	WITH_ACTIVE(s, freerdp_input_send_focus_in_event(s->context->input, 0));
}

static UINT16 clamp16(int32_t v)
{
	return (UINT16)MAX(0, MIN(65535, v));
}

void ferrum_input_mouse_move(ferrum_session* s, int32_t x, int32_t y)
{
	WITH_ACTIVE(s, freerdp_client_send_button_event((rdpClientContext*)s->context, FALSE,
	                                                PTR_FLAGS_MOVE, clamp16(x), clamp16(y)));
}

void ferrum_input_mouse_rel(ferrum_session* s, int32_t dx, int32_t dy)
{
	dx = MAX(-32768, MIN(32767, dx));
	dy = MAX(-32768, MIN(32767, dy));
	WITH_ACTIVE(s, freerdp_client_send_button_event((rdpClientContext*)s->context, TRUE,
	                                                PTR_FLAGS_MOVE, dx, dy));
}

void ferrum_input_mouse_button(ferrum_session* s, uint32_t button, bool down, int32_t x,
                               int32_t y)
{
	UINT16 flags = 0;
	BOOL extended = FALSE;
	switch (button)
	{
		case 1:
			flags = PTR_FLAGS_BUTTON1;
			break;
		case 2:
			flags = PTR_FLAGS_BUTTON3; /* RDP: BUTTON3 is middle */
			break;
		case 3:
			flags = PTR_FLAGS_BUTTON2;
			break;
		case 8:
			flags = PTR_XFLAGS_BUTTON1;
			extended = TRUE;
			break;
		case 9:
			flags = PTR_XFLAGS_BUTTON2;
			extended = TRUE;
			break;
		default:
			return;
	}
	if (down)
		flags |= extended ? PTR_XFLAGS_DOWN : PTR_FLAGS_DOWN;

	if (extended)
		WITH_ACTIVE(s, freerdp_client_send_extended_button_event(
		                   (rdpClientContext*)s->context, FALSE, flags, clamp16(x), clamp16(y)));
	else
		WITH_ACTIVE(s, freerdp_client_send_button_event((rdpClientContext*)s->context, FALSE,
		                                                flags, clamp16(x), clamp16(y)));
}

void ferrum_input_wheel(ferrum_session* s, bool horizontal, int32_t delta,
                        WINPR_ATTR_UNUSED int32_t x, WINPR_ATTR_UNUSED int32_t y)
{
	if (!s || delta == 0)
		return;
	/* RDP horizontal wheel is inverted relative to vertical */
	if (horizontal)
		delta = -delta;
	EnterCriticalSection(&s->lock);
	while (s->active && delta != 0)
	{
		const int32_t step = MAX(-0xFF, MIN(0xFF, delta));
		UINT16 flags = horizontal ? PTR_FLAGS_HWHEEL : PTR_FLAGS_WHEEL;
		if (step < 0)
			flags |= PTR_FLAGS_WHEEL_NEGATIVE | (UINT16)((0x100 + step) & 0xFF);
		else
			flags |= (UINT16)(step & 0xFF);
		freerdp_client_send_wheel_event((rdpClientContext*)s->context, flags);
		delta -= step;
	}
	LeaveCriticalSection(&s->lock);
}
