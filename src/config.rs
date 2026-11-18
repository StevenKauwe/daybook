use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    pub endpoint: String,
    pub model: String,
    pub remote_opt_in: bool,
    pub supports_images: bool,
    pub keychain_account: String,
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8080/v1".into(),
            model: String::new(),
            remote_opt_in: false,
            supports_images: false,
            keychain_account: String::new(),
            input_usd_per_million: None,
            output_usd_per_million: None,
        }
    }
}
impl Provider {
    pub fn local(&self) -> bool {
        url::Url::parse(&self.endpoint)
            .ok()
            .is_some_and(|u| matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "::1")))
    }
    pub fn validate(&self) -> Result<(), String> {
        for price in [self.input_usd_per_million, self.output_usd_per_million]
            .into_iter()
            .flatten()
        {
            if !price.is_finite() || !(0.0..=10000.0).contains(&price) {
                return Err(
                    "model prices must be finite, nonnegative USD per million tokens".into(),
                );
            }
        }
        let u = url::Url::parse(&self.endpoint).map_err(|_| "invalid provider URL")?;
        if !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
        {
            return Err("URLs cannot contain credentials, query, or fragment".into());
        }
        if self.local() {
            if !matches!(u.scheme(), "http" | "https") {
                return Err("invalid local protocol".into());
            }
        } else if !self.remote_opt_in || u.scheme() != "https" {
            return Err("hosted providers require explicit remote_opt_in and HTTPS; use literal 127.0.0.1 for local".into());
        }
        if self.model.len() > 200 || self.keychain_account.len() > 200 {
            return Err("provider field too long".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model_path: String,
    pub audio_storage: String,
    pub inference_backend: String,
    pub max_inference_work_mb: usize,
    pub inference_cooldown_ms: u64,
    pub screen_seconds: u64,
    pub compression_seconds: u64,
    pub rolling_audio_seconds: u64,
    pub retain_audio_hours: u64,
    pub retain_images: bool,
    pub image_hours: u64,
    pub disk_limit_mb: u64,
    pub max_pending: usize,
    pub daily_requests: u64,
    pub daily_images: u64,
    pub daily_token_reservation: u64,
    pub max_output_tokens: u64,
    pub summary_warn_chars: usize,
    pub work_start_hour: u32,
    pub work_end_hour: u32,
    pub weekdays_only: bool,
    pub meeting_bundle_id: String,
    pub screen_mode: String,
    pub summary: Provider,
    pub vision: Provider,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            audio_storage: "memory".into(),
            inference_backend: "auto".into(),
            max_inference_work_mb: 256,
            inference_cooldown_ms: 500,
            screen_seconds: 300,
            compression_seconds: 900,
            rolling_audio_seconds: 600,
            retain_audio_hours: 0,
            retain_images: false,
            image_hours: 24,
            disk_limit_mb: 1024,
            max_pending: 120,
            daily_requests: 100,
            daily_images: 24,
            daily_token_reservation: 100000,
            max_output_tokens: 1024,
            summary_warn_chars: 20000,
            work_start_hour: 8,
            work_end_hour: 18,
            weekdays_only: true,
            meeting_bundle_id: String::new(),
            screen_mode: "ocr".into(),
            summary: Provider::default(),
            vision: Provider::default(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !["memory", "recovery"].contains(&self.audio_storage.as_str())
            || (self.audio_storage == "memory" && self.retain_audio_hours > 0)
        {
            return Err("audio storage must be memory or recovery; longer audio retention requires recovery mode".into());
        }
        if !["auto", "cpu"].contains(&self.inference_backend.as_str()) {
            return Err("inference backend must be auto or cpu".into());
        }
        if !(64..=1024).contains(&self.max_inference_work_mb)
            || !(100..=10000).contains(&self.inference_cooldown_ms)
        {
            return Err("inference memory must be 64..1024 MiB; cooldown 100..10000 ms".into());
        }
        if !(30..=86400).contains(&self.screen_seconds)
            || !(60..=86400).contains(&self.compression_seconds)
        {
            return Err("screen interval must be 30..86400 seconds; compression 60..86400".into());
        }
        if !(30..=86400).contains(&self.rolling_audio_seconds)
            || self.retain_audio_hours > 720
            || self.image_hours > 720
        {
            return Err("invalid retention window".into());
        }
        if !(64..=102400).contains(&self.disk_limit_mb) || !(2..=1000).contains(&self.max_pending) {
            return Err("disk limit 64..102400 MiB and pending 2..1000 required".into());
        }
        if self.work_start_hour >= self.work_end_hour || self.work_end_hour > 24 {
            return Err("work hours must be increasing within 0..24".into());
        }
        if !(1..=8192).contains(&self.max_output_tokens)
            || self.daily_requests > 10000
            || self.daily_images > 1000
            || self.daily_token_reservation > 10000000
        {
            return Err("invalid provider budget".into());
        }
        if !["ocr", "vision", "both", "off"].contains(&self.screen_mode.as_str()) {
            return Err("screen_mode must be ocr, vision, both, or off".into());
        }
        self.summary.validate()?;
        self.vision.validate()?;
        if ["vision", "both"].contains(&self.screen_mode.as_str())
            && (!self.vision.supports_images || self.vision.model.is_empty())
        {
            return Err("vision needs an explicitly image-capable model".into());
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let mut c = Self::default();
            let model = path
                .parent()
                .unwrap_or(Path::new("."))
                .join("models/moonshine-tiny.gguf");
            let streaming = path
                .parent()
                .unwrap_or(Path::new("."))
                .join("models/moonshine-streaming-small-Q8_0.gguf");
            let medium = path
                .parent()
                .unwrap_or(Path::new("."))
                .join("models/moonshine-streaming-medium-Q8_0.gguf");
            if medium.is_file() {
                c.model_path = medium.to_string_lossy().into_owned();
            } else if streaming.is_file() {
                c.model_path = streaming.to_string_lossy().into_owned();
            } else if model.is_file() {
                c.model_path = model.to_string_lossy().into_owned();
            }
            return Ok(c);
        }
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let mut c: Self = serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
        if raw.get("audio_storage").is_none() && c.retain_audio_hours > 0 {
            c.audio_storage = "recovery".into();
        }
        c.validate()?;
        Ok(c)
    }
}
/// Import metadata only. Never evaluate {env:...}/{file:...} or copy apiKey/auth.json.
pub fn import_opencode(v: &serde_json::Value) -> Result<Provider, String> {
    let selection = v["model"]
        .as_str()
        .ok_or("OpenCode model must be provider/model")?;
    let (provider, model) = selection.split_once('/').ok_or("expected provider/model")?;
    let modern = !v["providers"][provider].is_null();
    let entry = if modern {
        &v["providers"][provider]
    } else {
        &v["provider"][provider]
    };
    let endpoint=entry[if modern {"settings"} else {"options"}]["baseURL"].as_str().ok_or("explicit providers.settings.baseURL (current) or provider.options.baseURL (V1) required; implicit endpoints are not guessed")?;
    if endpoint.contains('{') {
        return Err("resolve endpoint references manually; secrets are never imported".into());
    }
    let mut p = Provider::default();
    p.endpoint = endpoint.into();
    p.model = model.into();
    // An imported remote URL remains disabled until edited and explicitly opted in.
    p.supports_images = entry["models"][model]["modalities"]["input"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "image"));
    Ok(p)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backend_defaults_and_model_selection_preserve_explicit_settings() {
        let old: Config =
            serde_json::from_value(serde_json::json!({"model_path":"chosen.gguf"})).unwrap();
        assert_eq!(old.inference_backend, "auto");
        assert_eq!(old.model_path, "chosen.gguf");
        let mut c = old;
        c.inference_backend = "cpu".into();
        assert!(c.validate().is_ok());
        c.inference_backend = "typo".into();
        assert!(c.validate().is_err());
        let root =
            std::env::temp_dir().join(format!("daybook-model-choice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(
            root.join("models/moonshine-streaming-medium-Q8_0.gguf"),
            b"fixture",
        )
        .unwrap();
        assert!(
            Config::load(&root.join("config.json"))
                .unwrap()
                .model_path
                .ends_with("streaming-medium-Q8_0.gguf")
        );
        std::fs::write(
            root.join("config.json"),
            br#"{"model_path":"custom.gguf","inference_backend":"cpu"}"#,
        )
        .unwrap();
        let c = Config::load(&root.join("config.json")).unwrap();
        assert_eq!(c.model_path, "custom.gguf");
        assert_eq!(c.inference_backend, "cpu");
    }
    #[test]
    fn validation() {
        let mut c = Config::default();
        assert!(c.validate().is_ok());
        c.screen_seconds = 0;
        assert!(c.validate().is_err());
        let mut p = Provider::default();
        p.endpoint = "https://example.com/v1".into();
        assert!(p.validate().is_err());
        p.remote_opt_in = true;
        assert!(p.validate().is_ok());
        p.endpoint = "https://secret@example.com".into();
        assert!(p.validate().is_err())
    }
    #[test]
    fn import_no_secrets() {
        let p=import_opencode(&serde_json::json!({"model":"local/test","provider":{"local":{"options":{"baseURL":"http://127.0.0.1:8080/v1","apiKey":"secret"}}}})).unwrap();
        assert!(p.keychain_account.is_empty());
        assert!(!serde_json::to_string(&p).unwrap().contains("secret"));
    }
    #[test]
    fn audio_storage_migration_preserves_explicit_retention() {
        let path =
            std::env::temp_dir().join(format!("daybook-audio-config-{}.json", std::process::id()));
        std::fs::write(
            &path,
            br#"{"retain_audio_hours":0,"rolling_audio_seconds":60}"#,
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.audio_storage, "memory");
        assert_eq!(c.rolling_audio_seconds, 60);
        std::fs::write(&path, br#"{"retain_audio_hours":24}"#).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.audio_storage, "recovery");
        assert_eq!(c.retain_audio_hours, 24);
        std::fs::write(
            &path,
            br#"{"audio_storage":"memory","retain_audio_hours":24}"#,
        )
        .unwrap();
        assert!(Config::load(&path).is_err());
        std::fs::write(&path, br#"{"audio_storage":"invalid"}"#).unwrap();
        assert!(Config::load(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
/// Minimal JSONC lexical normalization: comments and trailing commas outside strings only.
pub fn parse_jsonc(input: &str) -> Result<serde_json::Value, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let (mut i, mut quoted, mut escape) = (0, false, false);
    while i < chars.len() {
        let ch = chars[i];
        if quoted {
            out.push(ch);
            if escape {
                escape = false
            } else if ch == '\\' {
                escape = true
            } else if ch == '"' {
                quoted = false
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            quoted = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'/') {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1
            }
            out.push(' ');
            continue;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1
            }
            if i + 1 >= chars.len() {
                return Err("unterminated JSONC comment".into());
            }
            i += 2;
            out.push(' ');
            continue;
        }
        out.push(ch);
        i += 1;
    }
    let chars: Vec<char> = out.chars().collect();
    let mut cleaned = String::new();
    quoted = false;
    escape = false;
    for (i, &ch) in chars.iter().enumerate() {
        if quoted {
            cleaned.push(ch);
            if escape {
                escape = false
            } else if ch == '\\' {
                escape = true
            } else if ch == '"' {
                quoted = false
            }
            continue;
        }
        if ch == '"' {
            quoted = true
        }
        if ch == ','
            && chars[i + 1..]
                .iter()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c == '}' || *c == ']')
        {
            continue;
        }
        cleaned.push(ch)
    }
    serde_json::from_str(&cleaned).map_err(|e| e.to_string())
}
#[cfg(test)]
mod import_tests {
    use super::*;
    #[test]
    fn modern_jsonc() {
        let v=parse_jsonc(r#"{// comment
 "model":"local/m", "providers":{"local":{"settings":{"baseURL":"http://127.0.0.1:8080/v1"},"models":{"m":{"modalities":{"input":["text","image",]}}},}},}
 "#).unwrap();
        let p = import_opencode(&v).unwrap();
        assert!(p.supports_images);
        assert!(p.local());
    }
    #[test]
    fn invalid_comment() {
        assert!(parse_jsonc("{/*").is_err())
    }
}
