//! Modal prompts raised by a session (credentials, certificates, ...).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use rdp_core::{AuthReason, CertVerdict, CertificateInfo, Credentials};

/// Wrap a `FnOnce` so it can be called from an `Fn` signal handler.
fn once<T: 'static>(f: impl FnOnce(T) + 'static) -> impl Fn(T) + 'static {
    let cell = Rc::new(RefCell::new(Some(f)));
    move |v| {
        if let Some(f) = cell.borrow_mut().take() {
            f(v)
        }
    }
}

fn markup_escape(s: &str) -> String {
    glib::markup_escape_text(s).to_string()
}

pub struct AuthAnswer {
    pub credentials: Credentials,
    pub remember: bool,
}

pub fn authenticate(
    parent: &impl IsA<gtk::Widget>,
    reason: AuthReason,
    current: Credentials,
    target: &str,
    can_remember: bool,
    reply: impl FnOnce(Option<AuthAnswer>) + 'static,
) -> adw::Dialog {
    let (heading, body) = if reason.is_gateway() {
        (
            "Gateway Sign In".to_string(),
            format!("Credentials for the gateway of {target}"),
        )
    } else if reason.is_pin() {
        (
            "Enter PIN".to_string(),
            "Unlock your security device".to_string(),
        )
    } else {
        ("Sign In".to_string(), format!("Credentials for {target}"))
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
    dialog.add_responses(&[("cancel", "_Cancel"), ("connect", "_Connect")]);
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("connect"));
    dialog.set_close_response("cancel");

    let group = adw::PreferencesGroup::new();
    let user = adw::EntryRow::builder()
        .title("User name")
        .text(&current.username)
        .build();
    let password = adw::PasswordEntryRow::builder()
        .title(if reason.is_pin() { "PIN" } else { "Password" })
        .build();
    password.set_text(&current.password);
    let domain = adw::EntryRow::builder()
        .title("Domain")
        .text(&current.domain)
        .build();
    let remember = adw::SwitchRow::builder().title("Remember password").build();
    if !reason.is_pin() {
        group.add(&user);
    }
    group.add(&password);
    if !reason.is_pin() {
        group.add(&domain);
    }
    if can_remember && !reason.is_pin() {
        group.add(&remember);
    }
    dialog.set_extra_child(Some(&group));

    // Enter in any field submits.
    user.set_activates_default(true);
    password.set_activates_default(true);
    domain.set_activates_default(true);

    let reply = once(reply);
    let (user_c, password_c) = (user.clone(), password.clone());
    dialog.connect_response(None, move |_, response| {
        let (user, password) = (&user_c, &password_c);
        if response == "connect" {
            let mut creds = Credentials {
                username: user.text().to_string(),
                password: password.text().to_string(),
                domain: domain.text().to_string(),
            };
            // Accept DOMAIN\user and user@domain in the user field.
            if creds.domain.is_empty() {
                if let Some((d, u)) = creds.username.clone().split_once('\\') {
                    creds.domain = d.into();
                    creds.username = u.into();
                }
            }
            reply(Some(AuthAnswer {
                credentials: creds,
                remember: remember.is_active(),
            }));
        } else {
            reply(None);
        }
    });
    dialog.present(Some(parent));
    if current.username.is_empty() && !reason.is_pin() {
        user.grab_focus();
    } else {
        password.grab_focus();
    }
    dialog.upcast()
}

pub fn certificate(
    parent: &impl IsA<gtk::Widget>,
    info: &CertificateInfo,
    reply: impl FnOnce(CertVerdict) + 'static,
) -> adw::Dialog {
    let changed = info.is_changed();
    let heading = if changed {
        "Certificate Changed"
    } else {
        "Unknown Certificate"
    };
    let mut body = if changed {
        format!(
            "The identity of <b>{}</b> has changed since the last connection. \
             This can mean someone is intercepting the connection.",
            markup_escape(&info.host)
        )
    } else {
        format!(
            "The identity of <b>{}</b> cannot be verified.",
            markup_escape(&info.host)
        )
    };
    if info.is_name_mismatch() {
        body.push_str("\n\nThe certificate was issued for a different name.");
    }
    if info.is_gateway() {
        body.push_str("\n\nThis is the certificate of the RD Gateway.");
    }

    let dialog = adw::AlertDialog::new(Some(heading), None);
    dialog.set_body_use_markup(true);
    dialog.set_body(&body);

    let details = adw::PreferencesGroup::new();
    let row = |title: &str, value: &str| {
        let r = adw::ActionRow::builder()
            .title(title)
            .subtitle(value)
            .subtitle_selectable(true)
            .build();
        r.add_css_class("property");
        r
    };
    details.add(&row("Host", &format!("{}:{}", info.host, info.port)));
    if !info.common_name.is_empty() {
        details.add(&row("Common name", &info.common_name));
    }
    details.add(&row("Subject", &info.subject));
    details.add(&row("Issuer", &info.issuer));
    details.add(&row("Fingerprint", &info.fingerprint));
    if let Some((_, _, old_fp)) = &info.old {
        details.add(&row("Previous fingerprint", old_fp));
    }
    dialog.set_extra_child(Some(&details));

    dialog.add_responses(&[
        ("reject", "_Reject"),
        ("once", "Accept _Once"),
        ("always", "_Trust"),
    ]);
    dialog.set_close_response("reject");
    dialog.set_default_response(Some("reject"));
    dialog.set_response_appearance(
        "always",
        if changed {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );

    let reply = once(reply);
    dialog.connect_response(None, move |_, response| {
        reply(match response {
            "always" => CertVerdict::AcceptPermanently,
            "once" => CertVerdict::AcceptOnce,
            _ => CertVerdict::Reject,
        })
    });
    dialog.present(Some(parent));
    dialog.upcast()
}

pub fn gateway_message(
    parent: &impl IsA<gtk::Widget>,
    message: &str,
    consent_mandatory: bool,
    reply: impl FnOnce(bool) + 'static,
) -> adw::Dialog {
    let dialog = adw::AlertDialog::new(Some("Gateway Message"), Some(message));
    if consent_mandatory {
        dialog.add_responses(&[("decline", "_Decline"), ("accept", "_Accept")]);
        dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("decline");
    } else {
        dialog.add_responses(&[("accept", "_OK")]);
        dialog.set_close_response("accept");
    }
    let reply = once(reply);
    dialog.connect_response(None, move |_, r| reply(r == "accept"));
    dialog.present(Some(parent));
    dialog.upcast()
}

pub fn choose_smartcard(
    parent: &impl IsA<gtk::Widget>,
    names: &[String],
    reply: impl FnOnce(Option<usize>) + 'static,
) -> adw::Dialog {
    let dialog = adw::AlertDialog::new(
        Some("Choose Certificate"),
        Some("Select the smartcard certificate to sign in with"),
    );
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    let mut first: Option<gtk::CheckButton> = None;
    let buttons: Vec<gtk::CheckButton> = names
        .iter()
        .map(|n| {
            let check = gtk::CheckButton::new();
            if let Some(f) = &first {
                check.set_group(Some(f));
            } else {
                check.set_active(true);
                first = Some(check.clone());
            }
            let row = adw::ActionRow::builder()
                .title(n)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            list.append(&row);
            check
        })
        .collect();
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[("cancel", "_Cancel"), ("ok", "_Use Certificate")]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_close_response("cancel");
    let reply = once(reply);
    dialog.connect_response(None, move |_, r| {
        reply(if r == "ok" {
            buttons.iter().position(|b| b.is_active())
        } else {
            None
        })
    });
    dialog.present(Some(parent));
    dialog.upcast()
}

pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    accept: &str,
    destructive: bool,
    reply: impl FnOnce(bool) + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "_Cancel"), ("ok", accept)]);
    dialog.set_response_appearance(
        "ok",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_close_response("cancel");
    let reply = once(reply);
    dialog.connect_response(None, move |_, r| reply(r == "ok"));
    dialog.present(Some(parent));
}
