//! Only credential references belong in the serializable settings draft.
use super::*;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Source {
    #[default]
    None,
    Keychain,
    Environment,
}

#[derive(Default)]
pub(super) struct Credentials {
    source: Source,
    pub secret: Zeroizing<String>,
}
impl Credentials {
    pub fn reset(&mut self, provider: &config::Provider) {
        self.secret.zeroize();
        self.source = if !provider.keychain_account.is_empty() {
            Source::Keychain
        } else if !provider.api_key_env.is_empty() {
            Source::Environment
        } else {
            Source::None
        };
    }
    pub fn show(&mut self, ui: &mut egui::Ui, p: &mut config::Provider) {
        let before = self.source;
        ui.label("API key source");
        ui.horizontal_wrapped(|ui| {
            for (source, label) in [
                (Source::None, "No key"),
                (Source::Keychain, "Mac Keychain"),
                (Source::Environment, "Environment variable"),
            ] {
                ui.selectable_value(&mut self.source, source, label);
            }
        });
        if self.source != before {
            self.secret.zeroize();
            p.keychain_account.clear();
            p.api_key_env.clear();
            match self.source {
                Source::Keychain => {
                    let host = url::Url::parse(&p.endpoint)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_owned));
                    p.keychain_account = format!(
                        "Daybook summary · {}",
                        host.as_deref().unwrap_or("provider")
                    );
                }
                Source::Environment => p.api_key_env = "DAYBOOK_API_KEY".into(),
                Source::None => {}
            }
        }
        match self.source {
            Source::None => {}
            Source::Keychain => {
                ui.label("API key");
                ui.add(
                    egui::TextEdit::singleline(&mut *self.secret)
                        .id_salt("daybook-provider-password")
                        .password(true)
                        .char_limit(8192)
                        .desired_width(f32::INFINITY)
                        .hint_text("Paste a new or replacement key"),
                );
                muted(
                    ui,
                    "Save changes stores the key in this Mac’s Keychain. Leave it blank to use the existing key.",
                );
                ui.collapsing("Keychain item", |ui| {
                    field(
                        ui,
                        "Account name",
                        &mut p.keychain_account,
                        "Existing or new account",
                    )
                });
            }
            Source::Environment => {
                field(ui, "Variable name", &mut p.api_key_env, "DAYBOOK_API_KEY");
                muted(
                    ui,
                    "Daybook reads this variable from its launch environment. A Dock launch uses the macOS login environment.",
                );
            }
        }
    }
    pub fn validate(&self, provider: &config::Provider) -> Result<(), String> {
        if self.source == Source::Environment && provider.api_key_env.is_empty() {
            return Err("Enter the name of the API key environment variable.".into());
        }
        if self.source == Source::Keychain && provider.keychain_account.is_empty() {
            return Err("Enter an account name under Keychain item.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_a_credential_keeps_the_draft_on_failure_and_clears_it_after_success() {
        let (tx, requests) = mpsc::sync_channel(6);
        let (replies, rx) = mpsc::sync_channel(6);
        let mut app = Controls::initial("synthetic://test".into(), tx, rx);
        app.loaded = true;
        app.config.summary.keychain_account = "Example item".into();
        app.credentials.reset(&app.config.summary);
        let settings = json!(app.config);
        app.saved_config = settings.clone();
        let key = "sample-only-api-key";
        app.credentials.secret.push_str(key);
        assert!(app.settings_dirty());
        app.submit_settings();
        let request = requests.try_recv().unwrap();
        assert_eq!(request.path, "/config_with_key");
        let body = request.body.unwrap();
        assert_eq!(body["config"], settings);
        assert_eq!(body["api_key"], key);
        assert!(!app.saved_config.to_string().contains(key));
        replies
            .send(Reply {
                path: request.path.clone(),
                body: Some(settings.clone()),
                result: Err("Keychain declined".into()),
            })
            .unwrap();
        app.poll();
        assert_eq!(app.credentials.secret.as_str(), key);
        assert!(!app.settings_saving);
        app.submit_settings();
        assert_eq!(requests.try_recv().unwrap().path, request.path);
        replies
            .send(Reply {
                path: request.path,
                body: Some(settings.clone()),
                result: Ok(json!({})),
            })
            .unwrap();
        app.poll();
        assert!(app.credentials.secret.is_empty());
        assert!(!app.settings_dirty());
        assert_eq!(app.saved_config, settings);
    }
}
