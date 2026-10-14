/*
 * ferrum shim: a narrow C API over libfreerdp3 consumed by Rust.
 *
 * All FreeRDP-specific structures stay on the C side. Rust only sees an
 * opaque session handle, plain-old-data structs and a table of callbacks.
 *
 * Threading:
 *  - ferrum_session_run() blocks and must be called from a dedicated thread.
 *  - Callbacks are invoked from the session thread or from FreeRDP channel
 *    threads (clipboard, rail). They may block (e.g. waiting for a dialog).
 *  - Input/resize/clipboard functions are thread safe and may be called from
 *    any thread (typically the UI thread).
 */
#ifndef FERRUM_SHIM_H
#define FERRUM_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ferrum_session ferrum_session;

enum ferrum_state
{
	FERRUM_STATE_CONNECTING = 0,
	FERRUM_STATE_CONNECTED = 1,
	FERRUM_STATE_RECONNECTING = 2,
	FERRUM_STATE_DISCONNECTED = 3,
};

enum ferrum_cursor_kind
{
	FERRUM_CURSOR_BITMAP = 0,
	FERRUM_CURSOR_HIDDEN = 1,
	FERRUM_CURSOR_DEFAULT = 2,
};

/* Return values of verify_certificate */
enum ferrum_cert_verdict
{
	FERRUM_CERT_REJECT = 0,
	FERRUM_CERT_ACCEPT_PERMANENT = 1,
	FERRUM_CERT_ACCEPT_ONCE = 2,
};

typedef struct
{
	const char* host;
	uint16_t port;
	const char* common_name;
	const char* subject;
	const char* issuer;
	const char* fingerprint;
	/* only set when the certificate changed */
	const char* old_subject;
	const char* old_issuer;
	const char* old_fingerprint;
	uint32_t flags; /* VERIFY_CERT_FLAG_* */
} ferrum_cert_info;

typedef struct
{
	uint32_t id;
	int32_t x, y, width, height;
	bool primary;
	uint32_t physical_width_mm, physical_height_mm;
	uint32_t orientation; /* degrees: 0, 90, 180, 270 */
	uint32_t desktop_scale; /* percent, 100..500 */
	uint32_t device_scale;  /* 100, 140, 180 */
} ferrum_monitor;

/* ---- RemoteApp (RAIL) ---- */
enum ferrum_rail_window_field
{
	FERRUM_RAIL_TITLE = 1 << 0,
	FERRUM_RAIL_OFFSET = 1 << 1,
	FERRUM_RAIL_SIZE = 1 << 2,
	FERRUM_RAIL_STATE = 1 << 3,
	FERRUM_RAIL_STYLE = 1 << 4,
	FERRUM_RAIL_OWNER = 1 << 5,
	FERRUM_RAIL_NEW = 1 << 6,
};

typedef struct
{
	uint64_t window_id;
	uint64_t owner_id;
	uint32_t fields; /* ferrum_rail_window_field: which values are valid */
	const char* title;
	/* window rectangle in remote desktop coordinates */
	int32_t x, y, width, height;
	uint32_t show_state; /* 0 hidden, 2 minimized, 3 maximized, 5 shown */
	uint32_t style, ex_style;
} ferrum_rail_window;

typedef struct
{
	void* user;

	void (*on_state)(void* user, int state);
	/* Desktop size changed (also called once after connect). */
	void (*on_desktop_resize)(void* user, uint32_t width, uint32_t height);
	/* A rectangle of the BGRX framebuffer changed. data points at pixel (0,0). */
	void (*on_frame)(void* user, const uint8_t* data, uint32_t stride, uint32_t width,
	                 uint32_t height, const int32_t* rects, size_t nrects);
	/* rects is an array of nrects*4 int32: x, y, w, h */
	void (*on_cursor)(void* user, int kind, const uint8_t* bgra, uint32_t width, uint32_t height,
	                  uint32_t hot_x, uint32_t hot_y);
	void (*on_cursor_position)(void* user, uint32_t x, uint32_t y);

	/* Credentials prompt. reason is rdp_auth_reason. Values are in/out: the
	 * callee frees nothing, and must replace values via ferrum_strdup(). */
	bool (*authenticate)(void* user, int reason, char** username, char** password,
	                     char** domain);
	int (*verify_certificate)(void* user, const ferrum_cert_info* info);
	bool (*gateway_message)(void* user, uint32_t type, bool display_mandatory,
	                        bool consent_mandatory, const char* message);
	/* returns index into names, or -1 to cancel */
	int (*choose_smartcard)(void* user, const char* const* names, size_t count);
	void (*logon_error)(void* user, const char* data, const char* type);

	/* Clipboard: the server offers these mime types (NUL-separated list). */
	void (*clip_remote_offer)(void* user, const char* const* mimes, size_t count);
	/* Reply to ferrum_clip_request_remote(). data==NULL means failure. */
	void (*clip_remote_data)(void* user, uint64_t request_id, const uint8_t* data,
	                         size_t len);
	/* The server wants local clipboard content of the given mime type.
	 * Blocking; return a buffer allocated with ferrum_malloc() or NULL. */
	uint8_t* (*clip_local_get)(void* user, const char* mime, size_t* len);

	/* RemoteApp */
	void (*rail_window)(void* user, const ferrum_rail_window* window);
	void (*rail_window_delete)(void* user, uint64_t window_id);
	void (*rail_window_icon)(void* user, uint64_t window_id, const uint8_t* bgra,
	                         uint32_t width, uint32_t height);
	void (*rail_exec_result)(void* user, uint32_t exec_result, uint32_t raw_result,
	                         const char* program);
	/* The server asks the client to move/resize a window locally (title bar drag).
	 * move_type: 1..8 = resize edges (RAIL_WMSZ_*), 9 = move, 10/11 = keyboard */
	void (*rail_local_move_size)(void* user, uint64_t window_id, bool start, uint32_t move_type,
	                             int32_t x, int32_t y);
} ferrum_callbacks;

/* ---- lifecycle ---- */
ferrum_session* ferrum_session_new(const ferrum_callbacks* callbacks);
void ferrum_session_free(ferrum_session* s);

/* Parse xfreerdp-style arguments (argv[0] is ignored). Returns 0 on success. */
int ferrum_session_parse_args(ferrum_session* s, int argc, const char* const* argv);
/* Set a single setting by FreeRDP name, e.g. ("ServerHostname", "host"). */
bool ferrum_session_set(ferrum_session* s, const char* name, const char* value);
/* Must be called before run. Coordinates are in local desktop space. */
bool ferrum_session_set_monitors(ferrum_session* s, const ferrum_monitor* monitors, size_t count);

/* Blocking: connect and pump events until disconnected or stopped.
 * Returns the FreeRDP last error code (0 = clean disconnect). */
uint32_t ferrum_session_run(ferrum_session* s);
/* Thread safe: abort connection attempt or disconnect. */
void ferrum_session_stop(ferrum_session* s);

const char* ferrum_error_name(uint32_t code);
const char* ferrum_error_string(uint32_t code);

/* ---- input (thread safe, ignored when not connected) ---- */
/* scancode: RDP scancode, bit 0x100 = extended (KBDEXT) */
void ferrum_input_key(ferrum_session* s, bool down, bool repeat, uint32_t scancode);
void ferrum_input_unicode(ferrum_session* s, bool down, uint16_t codepoint);
void ferrum_input_sync_locks(ferrum_session* s, bool scroll, bool num, bool caps, bool kana);
void ferrum_input_mouse_move(ferrum_session* s, int32_t x, int32_t y);
void ferrum_input_mouse_rel(ferrum_session* s, int32_t dx, int32_t dy);
/* button: 1 left, 2 middle, 3 right, 8 back(x1), 9 forward(x2) */
void ferrum_input_mouse_button(ferrum_session* s, uint32_t button, bool down, int32_t x,
                               int32_t y);
/* delta in wheel units (120 = one notch). positive = up / right */
void ferrum_input_wheel(ferrum_session* s, bool horizontal, int32_t delta, int32_t x, int32_t y);
void ferrum_input_focus_in(ferrum_session* s);

/* ---- display ---- */
void ferrum_display_resize(ferrum_session* s, uint32_t width, uint32_t height, uint32_t scale);
void ferrum_display_layout(ferrum_session* s, const ferrum_monitor* monitors, size_t count);
void ferrum_refresh(ferrum_session* s);
void ferrum_suppress_output(ferrum_session* s, bool suppress);

/* ---- clipboard ---- */
/* Local clipboard changed: announce mime types (count 0 = empty clipboard). */
void ferrum_clip_local_offer(ferrum_session* s, const char* const* mimes, size_t count);
/* Ask the server for the content of a previously offered mime type. */
void ferrum_clip_request_remote(ferrum_session* s, const char* mime, uint64_t request_id);

/* ---- RemoteApp ---- */
void ferrum_rail_activate(ferrum_session* s, uint64_t window_id, bool active);
/* command: SC_MINIMIZE=0xF020, SC_MAXIMIZE=0xF030, SC_CLOSE=0xF060, SC_RESTORE=0xF120 */
void ferrum_rail_syscommand(ferrum_session* s, uint64_t window_id, uint16_t command);
void ferrum_rail_move(ferrum_session* s, uint64_t window_id, int32_t left, int32_t top,
                      int32_t right, int32_t bottom);
void ferrum_rail_exec(ferrum_session* s, const char* program, const char* workdir,
                      const char* args);

/* ---- memory helpers (so Rust and C agree on the allocator) ---- */
void* ferrum_malloc(size_t size);
char* ferrum_strdup(const char* str);
void ferrum_free(void* ptr);

#ifdef __cplusplus
}
#endif

#endif
