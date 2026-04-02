use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;
use gpui_component::Root;

use crate::ipc_client::{self, VpnProfile};
use crate::keychain;

/// Open the settings window from the GPUI app context
pub fn open_settings(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(640.), px(600.)), cx);
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("FortiVPN Settings".into()),
                ..Default::default()
            }),
            focus: true,
            is_movable: true,
            is_resizable: false,
            ..Default::default()
        },
        |window, cx| {
            // gpui-component requires Root as the window's root view
            let settings_view = cx.new(|cx| SettingsView::new(window, cx));
            let view: AnyView = settings_view.into();
            cx.new(|cx| Root::new(view, window, cx))
        },
    );
}

struct SettingsView {
    profiles: Vec<VpnProfile>,
    selected_index: Option<usize>,
    name_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    username_input: Entity<InputState>,
    cert_input: Entity<InputState>,
    password_input: Entity<InputState>,
    status_message: Option<String>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let profiles = ipc_client::get_profiles();
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Profile name"));
        let host_input = cx.new(|cx| InputState::new(window, cx).placeholder("vpn.example.com"));
        let port_input = cx.new(|cx| InputState::new(window, cx).placeholder("443"));
        let username_input = cx.new(|cx| InputState::new(window, cx).placeholder("username"));
        let cert_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("SHA256 fingerprint (optional)"));
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Password")
        });

        let mut view = Self {
            profiles,
            selected_index: None,
            name_input,
            host_input,
            port_input,
            username_input,
            cert_input,
            password_input,
            status_message: None,
        };

        if !view.profiles.is_empty() {
            view.selected_index = Some(0);
            view.load_profile(0, window, cx);
        }

        view
    }

    fn load_profile(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let profile = &self.profiles[index];
        self.name_input.update(cx, |state, cx| {
            state.set_value(profile.name.clone(), window, cx);
        });
        self.host_input.update(cx, |state, cx| {
            state.set_value(profile.host.clone(), window, cx);
        });
        self.port_input.update(cx, |state, cx| {
            state.set_value(profile.port.to_string(), window, cx);
        });
        self.username_input.update(cx, |state, cx| {
            state.set_value(profile.username.clone(), window, cx);
        });
        self.cert_input.update(cx, |state, cx| {
            state.set_value(profile.trusted_cert.clone(), window, cx);
        });
        let pw = keychain::read_password(&profile.id).unwrap_or_default();
        self.password_input.update(cx, |state, cx| {
            state.set_value(pw, window, cx);
        });
    }

    fn clear_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in [
            &self.name_input,
            &self.host_input,
            &self.username_input,
            &self.cert_input,
            &self.password_input,
        ] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.port_input
            .update(cx, |s, cx| s.set_value("443", window, cx));
    }

    fn save_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).value().to_string();
        let host = self.host_input.read(cx).value().to_string();
        let port_str = self.port_input.read(cx).value().to_string();
        let username = self.username_input.read(cx).value().to_string();
        let trusted_cert = self.cert_input.read(cx).value().to_string();
        let password = self.password_input.read(cx).value().to_string();

        if name.is_empty() || host.is_empty() {
            self.status_message = Some("Name and host are required".into());
            cx.notify();
            return;
        }

        let port: u16 = port_str.parse().unwrap_or(443);

        let id = self
            .selected_index
            .and_then(|i| self.profiles.get(i))
            .map(|p| p.id.clone());

        let mut json = serde_json::json!({
            "name": name,
            "host": host,
            "port": port,
            "username": username,
            "trusted_cert": trusted_cert,
        });
        if let Some(id) = &id {
            json["id"] = serde_json::Value::String(id.clone());
        }

        match ipc_client::save_profile(&json) {
            Some(resp) if resp.ok => {
                let profile_id = if let Some(id) = id {
                    id
                } else if let Some(data) = &resp.data {
                    data.get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    String::new()
                };

                if !password.is_empty() && !profile_id.is_empty() {
                    let _ = keychain::store_password(&profile_id, &password);
                }

                self.status_message = Some("Saved".into());
                self.reload_profiles(window, cx);
            }
            Some(resp) => {
                self.status_message = Some(format!("Error: {}", resp.message));
            }
            None => {
                self.status_message = Some("Daemon not responding".into());
            }
        }
        cx.notify();
    }

    fn delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index else {
            return;
        };
        let Some(profile) = self.profiles.get(index) else {
            return;
        };

        if let Some(resp) = ipc_client::delete_profile(&profile.id) {
            if resp.ok {
                self.status_message = Some("Deleted".into());
                self.selected_index = None;
                self.reload_profiles(window, cx);
                self.clear_form(window, cx);
            } else {
                self.status_message = Some(format!("Error: {}", resp.message));
            }
        }
        cx.notify();
    }

    fn reload_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profiles = ipc_client::get_profiles();
        if let Some(idx) = self.selected_index {
            if idx < self.profiles.len() {
                self.load_profile(idx, window, cx);
            } else if !self.profiles.is_empty() {
                self.selected_index = Some(0);
                self.load_profile(0, window, cx);
            } else {
                self.selected_index = None;
            }
        }
    }

    fn fetch_certificate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host_input.read(cx).value().to_string();
        let port_str = self.port_input.read(cx).value().to_string();
        let port: u16 = port_str.parse().unwrap_or(443);

        if host.is_empty() {
            self.status_message = Some("Enter host first".into());
            cx.notify();
            return;
        }

        self.status_message = Some("Fetching certificate...".into());
        cx.notify();

        match fetch_cert_fingerprint(&host, port) {
            Ok(fp) => {
                self.cert_input
                    .update(cx, |s, cx| s.set_value(fp, window, cx));
                self.status_message = Some("Certificate fetched".into());
            }
            Err(e) => {
                self.status_message = Some(e);
            }
        }
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bg = theme.background;
        let sidebar_bg = theme.sidebar;
        let border = theme.border;
        let fg = theme.foreground;
        let muted = theme.muted_foreground;
        let accent = theme.accent;

        div()
            .flex()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(self.render_sidebar(sidebar_bg, border, muted, accent, cx))
            .child(self.render_form(muted, cx))
    }
}

impl SettingsView {
    fn render_sidebar(
        &self,
        sidebar_bg: Hsla,
        border: Hsla,
        _muted: Hsla,
        accent: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w(px(180.))
            .h_full()
            .flex()
            .flex_col()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Profiles"),
            )
            .child({
                let profiles: Vec<_> = self
                    .profiles
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (i, p.id.clone(), p.name.clone()))
                    .collect();
                let selected_index = self.selected_index;
                div()
                    .id("profile-list")
                    .flex_1()
                    .overflow_scroll()
                    .children(profiles.into_iter().map(|(i, id, name)| {
                        let is_selected = selected_index == Some(i);
                        let has_pw = keychain::has_password(&id);
                        let dot = if has_pw { "\u{25CF}" } else { "\u{25CB}" };
                        let dot_color = if has_pw {
                            rgb(0x22C55E) // green
                        } else {
                            rgb(0xEF4444) // red
                        };

                        let mut item = div()
                            .id(ElementId::Name(format!("profile-{i}").into()))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py(px(6.))
                            .cursor_pointer();
                        if is_selected {
                            item = item.bg(accent);
                        }
                        item.on_click(cx.listener(move |this, _, window, cx| {
                            this.selected_index = Some(i);
                            this.load_profile(i, window, cx);
                            this.status_message = None;
                            cx.notify();
                        }))
                        .child(div().text_xs().text_color(dot_color).child(dot.to_string()))
                        .child(
                            div()
                                .text_sm()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(name),
                        )
                    }))
            })
            .child(
                div().border_t_1().border_color(border).p_2().child(
                    Button::new("new-profile")
                        .label("+ New Profile")
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.selected_index = None;
                            this.clear_form(window, cx);
                            this.status_message = None;
                            cx.notify();
                        })),
                ),
            )
    }

    fn render_form(&self, muted: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-form")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .overflow_scroll()
            // Title
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(
                if self.selected_index.is_some() {
                    "Edit Profile"
                } else {
                    "New Profile"
                },
            ))
            // Form fields
            .child(form_field("Name", &self.name_input))
            .child(form_field("Host", &self.host_input))
            .child(form_field("Port", &self.port_input))
            .child(form_field("Username", &self.username_input))
            // Certificate with Fetch button
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("Certificate SHA256"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().child(Input::new(&self.cert_input)))
                            .child(Button::new("fetch-cert").label("Fetch").compact().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.fetch_certificate(window, cx);
                                }),
                            )),
                    ),
            )
            // Password (masked)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(muted).child("Password"))
                    .child(Input::new(&self.password_input)),
            )
            // Status message
            .when_some(self.status_message.clone(), |this, msg| {
                this.child(div().text_sm().text_color(muted).child(msg))
            })
            // Spacer
            .child(div().flex_1())
            // Action buttons
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(if self.selected_index.is_some() {
                        Button::new("delete")
                            .label("Delete")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delete_profile(window, cx);
                            }))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(Button::new("save").label("Save").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.save_profile(window, cx);
                        },
                    ))),
            )
    }
}

/// Fetch TLS certificate SHA256 fingerprint using pure Rust (no shell commands)
fn fetch_cert_fingerprint(host: &str, port: u16) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = format!("{host}:{port}");
    let tcp = TcpStream::connect(&addr).map_err(|e| format!("Cannot connect to {addr}: {e}"))?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok();

    // Use rustls to do TLS handshake and extract the server certificate
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Accept any certificate (we want the fingerprint, not to validate it)
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAnyCert))
        .with_no_client_auth();

    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("Invalid hostname: {e}"))?;

    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|e| format!("TLS error: {e}"))?;

    let mut tcp_ref = &tcp;
    let mut sock = rustls::Stream::new(&mut conn, &mut tcp_ref);

    // Trigger the handshake by attempting to write
    let _ = sock.write_all(b"");
    let _ = sock.flush();
    // Read a bit to complete handshake
    let mut buf = [0u8; 1];
    let _ = sock.read(&mut buf);

    // Extract peer certificates
    let certs = conn
        .peer_certificates()
        .ok_or("No certificates from server")?;
    let cert_der = certs.first().ok_or("Empty certificate chain")?;

    // Compute SHA256 fingerprint
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(cert_der.as_ref());
    let fp = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(fp)
}

/// Accepts any TLS certificate (used for fingerprint fetching, not for VPN connections)
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn form_field(label: &str, input: &Entity<InputState>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x9CA3AF))
                .child(label.to_string()),
        )
        .child(Input::new(input))
}
