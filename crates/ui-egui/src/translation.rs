//! Translation of selected text. The UI owns only the request state; the provider is kept behind
//! a small trait so requests can be mocked without contacting a service.
//!
//! The default provider needs no API key. For every piece of text it tries, in order:
//! 1. DeepL's web endpoint (best quality, unofficial, often rate-limited),
//! 2. the internal endpoint the Google Translate website uses,
//! 3. the older public Google endpoint.
//!
//! Set the environment variable `PDFCRAFT_TRANSLATE_DEBUG=1` to print which service answered, or
//! why each one failed, to stderr.

use std::sync::mpsc::{self, Receiver};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

/// Languages shown by default.
pub const LANGUAGES: &[(&str, &str)] = &[("auto", "Detect automatically"), ("en", "English"), ("fr", "French"), ("ar", "Arabic")];

/// Languages the user can add on demand with `TranslationView::add_language`.
pub const MORE_LANGUAGES: &[(&str, &str)] = &[
    ("zh", "Chinese"),
    ("nl", "Dutch"),
    ("de", "German"),
    ("hi", "Hindi"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("es", "Spanish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
];

/// Longest piece of text sent in one request (the legacy Google endpoint takes it in the URL).
const MAX_CHUNK_CHARS: usize = 1500;

#[derive(Debug)]
pub struct TranslationView {
    pub open: bool,
    pub original: String,
    pub translated: Option<String>,
    pub source: String,
    pub target: String,
    pub document: String,
    pub page: usize,
    pub error: Option<String>,
    pub saved: bool,
    pub save_requested: bool,
    /// Extra languages the user added on top of `LANGUAGES`.
    pub extra_languages: Vec<(String, String)>,
    request: Option<Receiver<Result<String, String>>>,
}

impl Default for TranslationView {
    fn default() -> Self {
        Self {
            open: false,
            original: String::new(),
            translated: None,
            source: "auto".into(),
            target: "en".into(),
            document: String::new(),
            page: 0,
            error: None,
            saved: false,
            save_requested: false,
            extra_languages: Vec::new(),
            request: None,
        }
    }
}

pub trait TranslationProvider: Send + 'static {
    fn translate(&self, text: &str, source: &str, target: &str) -> Result<String, String>;
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct WebTranslateProvider {
    /// DeepL's web endpoint (best quality, undocumented, may be rate-limited or change).
    pub deepl_endpoint: String,
    /// The endpoint the Google Translate website uses (undocumented, may change).
    pub google_web_endpoint: String,
    /// The older public Google endpoint, the last fallback.
    pub google_legacy_endpoint: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for WebTranslateProvider {
    fn default() -> Self {
        Self {
            deepl_endpoint: "https://www2.deepl.com/jsonrpc".into(),
            google_web_endpoint: "https://translate.google.com/_/TranslateWebserverUi/data/batchexecute".into(),
            google_legacy_endpoint: "https://translate.googleapis.com/translate_a/single".into(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl TranslationProvider for WebTranslateProvider {
    fn translate(&self, text: &str, source: &str, target: &str) -> Result<String, String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots()?).build())
            .build()
            .new_agent();
        let mut output = String::new();
        for chunk in chunk_text(text, MAX_CHUNK_CHARS) {
            let mut errors = Vec::new();
            let piece = note("DeepL", deepl_translate(&agent, &self.deepl_endpoint, &chunk, source, target), &mut errors)
                .or_else(|| note("Google web", web_translate(&agent, &self.google_web_endpoint, &chunk, source, target), &mut errors))
                .or_else(|| note("Google", legacy_translate(&agent, &self.google_legacy_endpoint, &chunk, source, target), &mut errors))
                .ok_or_else(|| format!("every translation service failed ({})", errors.join("; ")))?;
            output.push_str(&piece);
        }
        let output = output.trim().to_owned();
        if output.is_empty() { Err("the translation service returned no translated text".into()) } else { Ok(output) }
    }
}

/// Records a failed attempt (and optionally logs the outcome); returns the text on success.
#[cfg(not(target_arch = "wasm32"))]
fn note(service: &str, result: Result<String, String>, errors: &mut Vec<String>) -> Option<String> {
    let debug = std::env::var_os("PDFCRAFT_TRANSLATE_DEBUG").is_some();
    match result {
        Ok(text) => {
            if debug {
                eprintln!("[translate] answered by {service}");
            }
            Some(text)
        }
        Err(error) => {
            if debug {
                eprintln!("[translate] {service} failed: {error}");
            }
            errors.push(format!("{service}: {error}"));
            None
        }
    }
}

/// Maps our language codes to DeepL's upper-case codes (`auto` stays as is).
fn deepl_language(code: &str) -> String {
    if code == "auto" { "auto".into() } else { code.to_ascii_uppercase() }
}

/// DeepL's web endpoint rejects requests whose timestamp does not match the number of `i`
/// characters in the text, so the timestamp is rounded up to a multiple of `count + 1`.
fn deepl_timestamp(text: &str, now_ms: u128) -> u128 {
    let count = text.chars().filter(|c| *c == 'i').count() as u128;
    if count == 0 { now_ms } else { now_ms - now_ms % (count + 1) + (count + 1) }
}

/// Builds the JSON-RPC body for DeepL's `LMT_handle_texts` method.
fn deepl_body(text: &str, source: &str, target: &str, id: u64, now_ms: u128) -> String {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "LMT_handle_texts",
        "id": id,
        "params": {
            "splitting": "newlines",
            "lang": {
                "source_lang_user_selected": deepl_language(source),
                "target_lang": deepl_language(target),
            },
            "texts": [{"text": text, "requestAlternatives": 0}],
            "timestamp": deepl_timestamp(text, now_ms),
            "commonJobParams": {"wasSpoken": false, "transcribe_as": ""},
        }
    })
    .to_string();
    // The service also expects an odd spacing quirk in the method field for some request ids.
    if (id + 3) % 13 == 0 || (id + 5) % 29 == 0 {
        body.replacen("\"method\":\"LMT_handle_texts\"", "\"method\" : \"LMT_handle_texts\"", 1)
    } else {
        body.replacen("\"method\":\"LMT_handle_texts\"", "\"method\": \"LMT_handle_texts\"", 1)
    }
}

/// Reads the translation out of DeepL's JSON-RPC answer.
fn parse_deepl_response(answer: &serde_json::Value) -> Result<String, String> {
    if let Some(message) = answer.pointer("/error/message").and_then(serde_json::Value::as_str) {
        return Err(format!("DeepL rejected the request: {message}"));
    }
    answer
        .pointer("/result/texts/0/text")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "DeepL returned no translated text".into())
}

/// Translates with DeepL's web (browser extension) endpoint.
#[cfg(not(target_arch = "wasm32"))]
fn deepl_translate(agent: &ureq::Agent, endpoint: &str, text: &str, source: &str, target: &str) -> Result<String, String> {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let id = 8_300_000 + (now_ms % 1_000_000) as u64;
    let body = deepl_body(text, source, target, id, now_ms);
    let mut response = agent
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "*/*")
        .header("accept-language", "en-US,en;q=0.9")
        .header("user-agent", "DeepL-iOS/2.9.1 iOS 16.3.0 (iPhone13,2)")
        .header("x-app-os-name", "iOS")
        .header("x-app-os-version", "16.3.0")
        .header("x-app-device", "iPhone13,2")
        .header("x-app-build", "510265")
        .header("x-app-version", "2.9.1")
        .header("referer", "https://www.deepl.com/")
        .send(body)
        .map_err(|e| format!("translation request failed: {e}"))?;
    let body = response.body_mut().with_config().limit(1 << 22).read_to_string().map_err(|e| format!("translation response was invalid: {e}"))?;
    let answer: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("translation response was invalid: {e}"))?;
    parse_deepl_response(&answer)
}

/// Translates with the Google Translate website's internal `batchexecute` endpoint (rpc `MkEWBc`).
#[cfg(not(target_arch = "wasm32"))]
fn web_translate(agent: &ureq::Agent, endpoint: &str, text: &str, source: &str, target: &str) -> Result<String, String> {
    let inner = serde_json::json!([[text, source, target, true], [null]]).to_string();
    let outer = serde_json::json!([[["MkEWBc", inner, null, "generic"]]]).to_string();
    let form = format!("f.req={}", url_encode(&outer));
    let request_id = 100_000 + std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() % 900_000);
    let mut response = agent
        .post(endpoint)
        .query("rpcids", "MkEWBc")
        .query("source-path", "/")
        .query("bl", "boq_translate-webserver_20201207.13_p0")
        .query("hl", "en")
        .query("soc-app", "1")
        .query("soc-platform", "1")
        .query("soc-device", "1")
        .query("_reqid", request_id.to_string())
        .query("rt", "c")
        .header("content-type", "application/x-www-form-urlencoded;charset=UTF-8")
        .header("user-agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
        .send(form)
        .map_err(|e| format!("translation request failed: {e}"))?;
    let body = response.body_mut().with_config().limit(1 << 22).read_to_string().map_err(|e| format!("translation response was invalid: {e}"))?;
    parse_web_response(&body)
}

/// Translates with the older public Google endpoint, splitting the text so the URL stays short.
#[cfg(not(target_arch = "wasm32"))]
fn legacy_translate(agent: &ureq::Agent, endpoint: &str, text: &str, source: &str, target: &str) -> Result<String, String> {
    let mut output = String::new();
    for chunk in chunk_text(text, MAX_CHUNK_CHARS) {
        let mut response = agent
            .get(endpoint)
            .query("client", "gtx")
            .query("sl", source)
            .query("tl", target)
            .query("dt", "t")
            .query("q", &chunk)
            .call()
            .map_err(|e| format!("translation request failed: {e}"))?;
        let body = response.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| format!("translation response was invalid: {e}"))?;
        let answer: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("translation response was invalid: {e}"))?;
        output.push_str(&parse_google_response(&answer)?);
    }
    Ok(output)
}

/// Percent-encodes a string for an `application/x-www-form-urlencoded` body.
#[cfg(not(target_arch = "wasm32"))]
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The `batchexecute` answer is a `)]}'` header followed by length-prefixed JSON lines. The line
/// holding `["wrb.fr","MkEWBc","<json string>", ...]` carries the translation: inside that inner
/// JSON, `[1][0][0][5]` is the list of translated sentences (`[translated, original, ...]`).
fn parse_web_response(body: &str) -> Result<String, String> {
    for line in body.lines().map(str::trim).filter(|line| line.starts_with("[[")) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(entries) = value.as_array() else { continue };
        for entry in entries {
            if entry.get(0).and_then(serde_json::Value::as_str) != Some("wrb.fr") {
                continue;
            }
            let Some(payload) = entry.get(2).and_then(serde_json::Value::as_str) else { continue };
            let inner: serde_json::Value = serde_json::from_str(payload).map_err(|e| format!("translation response was invalid: {e}"))?;
            let segments =
                inner.pointer("/1/0/0/5").and_then(serde_json::Value::as_array).ok_or("Google Translate returned an unexpected response")?;
            let mut text = String::new();
            for segment in segments {
                let Some(translated) = segment.get(0).and_then(serde_json::Value::as_str) else { continue };
                text.push_str(translated);
                // Keep the spacing between sentences that the original text had.
                if let Some(original) = segment.get(1).and_then(serde_json::Value::as_str) {
                    let trailing: String = original.chars().rev().take_while(|c| c.is_whitespace()).collect::<Vec<_>>().into_iter().rev().collect();
                    if !trailing.is_empty() && !translated.ends_with(char::is_whitespace) {
                        text.push_str(&trailing);
                    }
                }
            }
            if text.trim().is_empty() {
                return Err("Google Translate returned no translated text".into());
            }
            return Ok(text);
        }
    }
    Err("Google Translate returned an unexpected response".into())
}

/// The legacy endpoint answers with nested arrays: `[[["translated", "original", ...], ...], ...]`.
/// Long text comes back as several segments that must be joined in order.
fn parse_google_response(answer: &serde_json::Value) -> Result<String, String> {
    let segments = answer.get(0).and_then(serde_json::Value::as_array).ok_or("Google Translate returned an unexpected response")?;
    let text: String = segments.iter().filter_map(|segment| segment.get(0).and_then(serde_json::Value::as_str)).collect();
    if text.trim().is_empty() { Err("Google Translate returned no translated text".into()) } else { Ok(text) }
}

/// Splits text into pieces of at most `max` characters, preferring line and word boundaries.
fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if current.chars().count() + line.chars().count() <= max {
            current.push_str(line);
            continue;
        }
        if !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        for word in line.split_inclusive(char::is_whitespace) {
            if current.chars().count() + word.chars().count() > max && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            // A single "word" longer than the limit is cut hard.
            let mut rest = word;
            while rest.chars().count() > max {
                let cut = rest.char_indices().nth(max).map_or(rest.len(), |(i, _)| i);
                chunks.push(rest[..cut].to_owned());
                rest = &rest[cut..];
            }
            current.push_str(rest);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(not(target_arch = "wasm32"))]
fn os_roots() -> Result<ureq::tls::RootCerts, String> {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> = found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
    if certs.is_empty() {
        return Err("no trusted certificates found on this system".into());
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

fn is_rtl_char(c: char) -> bool {
    matches!(c, '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
}

/// True when most letters in `text` belong to a right-to-left script (Arabic, Hebrew, etc.).
pub fn is_rtl(text: &str) -> bool {
    let (mut rtl, mut other) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        if is_rtl_char(c) {
            rtl += 1;
        } else {
            other += 1;
        }
    }
    rtl > other
}

/// Text ready to draw in a renderer that lays text out strictly left to right.
/// Left-to-right text is returned unchanged. For right-to-left text the word order of each line
/// is reversed, so it reads correctly. Use for display only; copy and save the original string.
#[cfg(test)]
pub fn display_text(text: &str) -> String {
    if !is_rtl(text) {
        return text.to_owned();
    }
    text.lines().map(|line| line.split_whitespace().rev().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n")
}

/// Draws the translation as read-only, selectable text (not an editable box). RTL text is put in
/// the same visual word order as the saved-translation hover and aligned to the right.
///
/// Call this instead of the editable output box: `translation::show_output(ui, text)`.
pub fn show_output(ui: &mut egui::Ui, text: &str) {
    let align = if is_rtl(text) { egui::Align::Max } else { egui::Align::Min };
    let displayed = crate::bidi::visual_auto(text).into_owned();
    ui.with_layout(egui::Layout::top_down(align), |ui| {
        ui.add(egui::Label::new(displayed).wrap().selectable(true));
    });
}

pub fn request<P: TranslationProvider>(provider: P, text: String, source: String, target: String) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = if text.trim().is_empty() {
            Err("there is no text to translate".into())
        } else if source != "auto" && source == target {
            Ok(text)
        } else {
            provider.translate(&text, &source, &target)
        };
        let _ = sender.send(result);
    });
    receiver
}

impl TranslationView {
    pub fn open(&mut self, original: String, document: String, page: usize) {
        self.open = true;
        self.original = original;
        self.document = document;
        self.page = page;
        self.translated = None;
        self.error = None;
        self.saved = false;
        self.save_requested = false;
        self.request = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.original.clear();
        self.translated = None;
        self.error = None;
        self.request = None;
        self.saved = false;
        self.save_requested = false;
    }

    pub fn poll(&mut self) {
        let Some(request) = &self.request else { return };
        match request.try_recv() {
            Ok(Ok(text)) => {
                self.translated = Some(text);
                self.error = None;
                self.request = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.request = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("translation worker stopped unexpectedly".into());
                self.request = None;
            }
        }
    }

    pub fn loading(&self) -> bool {
        self.request.is_some()
    }

    /// All languages currently selectable: the defaults plus any the user added.
    pub fn languages(&self) -> Vec<(String, String)> {
        LANGUAGES.iter().map(|(code, name)| (code.to_string(), name.to_string())).chain(self.extra_languages.iter().cloned()).collect()
    }

    pub fn start(&mut self) {
        self.translated = None;
        self.error = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.request = Some(request(WebTranslateProvider::default(), self.original.clone(), self.source.clone(), self.target.clone()));
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.error = Some("translation providers are not configured for the web build".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock;
    impl TranslationProvider for Mock {
        fn translate(&self, text: &str, _source: &str, target: &str) -> Result<String, String> {
            Ok(format!("{target}:{text}"))
        }
    }

    struct Failing;
    impl TranslationProvider for Failing {
        fn translate(&self, _text: &str, _source: &str, _target: &str) -> Result<String, String> {
            Err("offline".into())
        }
    }

    #[test]
    fn mocked_provider_translates_without_network() {
        let result = request(Mock, "hello".into(), "en".into(), "fr".into()).recv().map_err(|e| e.to_string()).and_then(|x| x);
        assert_eq!(result.as_deref(), Ok("fr:hello"));
    }

    #[test]
    fn empty_translation_is_rejected() {
        let result = request(Mock, " ".into(), "auto".into(), "en".into()).recv().map_err(|e| e.to_string()).and_then(|x| x);
        assert_eq!(result, Err("there is no text to translate".into()));
    }

    #[test]
    fn provider_errors_are_returned_to_the_popup_layer() {
        let result = request(Failing, "hello".into(), "auto".into(), "en".into()).recv().map_err(|e| e.to_string()).and_then(|x| x);
        assert_eq!(result, Err("offline".into()));
    }

    #[test]
    fn google_response_segments_are_joined() {
        let response = serde_json::json!([[["Bonjour. ", "Hello. ", null, null, 10], ["Ça va ?", "How are you?", null, null, 10]], null, "en"]);
        assert_eq!(parse_google_response(&response).as_deref(), Ok("Bonjour. Ça va ?"));
    }

    #[test]
    fn unexpected_google_response_is_an_error() {
        let response = serde_json::json!({"error": "blocked"});
        assert!(parse_google_response(&response).is_err());
    }

    #[test]
    fn web_response_is_read_from_batchexecute_payload() {
        let segments = serde_json::json!([["Bonjour.", "Hello. "], ["Ça va ?", "How are you?"]]);
        let inner = serde_json::json!([null, [[[null, null, null, null, null, segments]]], "en"]).to_string();
        let line = serde_json::json!([["wrb.fr", "MkEWBc", inner, null, null, null, "generic"]]).to_string();
        let body = format!(")]}}'\n\n{}\n{line}\n25\n[[\"di\",12]]\n", line.len());
        assert_eq!(parse_web_response(&body).as_deref(), Ok("Bonjour. Ça va ?"));
    }

    #[test]
    fn unexpected_web_response_is_an_error() {
        assert!(parse_web_response(")]}'\n\n12\n[[\"di\",12]]\n").is_err());
        assert!(parse_web_response("<html>blocked</html>").is_err());
    }

    #[test]
    fn deepl_language_codes_are_upper_case() {
        assert_eq!(deepl_language("auto"), "auto");
        assert_eq!(deepl_language("ar"), "AR");
        assert_eq!(deepl_language("fr"), "FR");
    }

    #[test]
    fn deepl_timestamp_matches_the_number_of_i_characters() {
        assert_eq!(deepl_timestamp("no letters here", 1_000), 1_000);
        // Three "i" characters: the timestamp becomes a multiple of 4.
        let stamp = deepl_timestamp("iii", 1_001);
        assert_eq!(stamp % 4, 0);
        assert!(stamp > 1_001 && stamp <= 1_005);
    }

    #[test]
    fn deepl_body_carries_text_languages_and_method_spacing() {
        let plain = deepl_body("hello", "en", "ar", 1, 5_000);
        assert!(plain.contains("\"method\": \"LMT_handle_texts\""));
        let value: serde_json::Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(value.pointer("/params/texts/0/text").and_then(serde_json::Value::as_str), Some("hello"));
        assert_eq!(value.pointer("/params/lang/target_lang").and_then(serde_json::Value::as_str), Some("AR"));
        // id 10 makes (id + 3) divisible by 13, which switches on the extra space.
        let spaced = deepl_body("hello", "auto", "fr", 10, 5_000);
        assert!(spaced.contains("\"method\" : \"LMT_handle_texts\""));
        assert!(serde_json::from_str::<serde_json::Value>(&spaced).is_ok());
    }

    #[test]
    fn deepl_response_is_read_or_reported() {
        let ok = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"texts": [{"text": " الكتاب مُوجَّه "}], "lang": "EN"}});
        assert_eq!(parse_deepl_response(&ok).as_deref(), Ok("الكتاب مُوجَّه"));
        let rate_limited = serde_json::json!({"error": {"code": 1042912, "message": "Too many requests"}});
        assert_eq!(parse_deepl_response(&rate_limited), Err("DeepL rejected the request: Too many requests".into()));
        assert!(parse_deepl_response(&serde_json::json!({"result": {"texts": []}})).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn form_values_are_percent_encoded() {
        assert_eq!(url_encode("a b&c=é"), "a%20b%26c%3D%C3%A9");
        assert_eq!(url_encode("safe-_.~09az"), "safe-_.~09az");
    }

    #[test]
    fn long_text_is_chunked_without_losing_content() {
        let text = "word ".repeat(1000);
        let chunks = chunk_text(&text, 100);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn latin_text_is_not_reordered() {
        assert_eq!(display_text("hello big world"), "hello big world");
    }

    #[test]
    fn arabic_word_order_is_flipped_per_line() {
        assert_eq!(display_text("مرحبا بك في العالم\nشكرا لك"), "العالم في بك مرحبا\nلك شكرا");
        assert!(is_rtl("مرحبا"));
    }

    #[test]
    fn closing_translation_clears_temporary_popup_state() {
        let mut view = TranslationView::default();
        view.open("hello".into(), "document.pdf".into(), 2);
        view.translated = Some("bonjour".into());
        view.close();
        assert!(!view.open);
        assert!(view.original.is_empty());
        assert!(view.translated.is_none());
    }
}
