//! Shared browser recording adapter for learning answers and Voice Notes.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Bounded browser audio ready for the generic upload protocol.
#[derive(Clone, PartialEq)]
pub(crate) struct RecordedAudio {
    /// Original bytes.
    pub(crate) bytes: Vec<u8>,
    /// Negotiated safe media type.
    pub(crate) media_type: String,
    /// Best-effort recording duration.
    pub(crate) duration_ms: Option<u64>,
}

impl RecordedAudio {
    pub(crate) fn from_file(name: &str, bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > lumi_core::MAX_LEARNING_AUDIO_BYTES as usize {
            return Err("Аудиофайл пуст или превышает 25 МиБ.".to_owned());
        }
        let media_type = media_type_for_file_name(name)
            .ok_or_else(|| "Поддерживаются WebM, OGG, M4A/MP4, MP3 и WAV.".to_owned())?;
        Ok(Self {
            bytes,
            media_type: media_type.to_owned(),
            duration_ms: None,
        })
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(inline_js = r#"
let lumiRecorder = null;
let lumiRecorderStream = null;
let lumiRecorderChunks = [];
let lumiRecorderStartedAt = 0;

export async function startLumiRecording() {
  if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
    throw new Error("media_recorder_unavailable");
  }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const candidates = ["audio/webm;codecs=opus", "audio/ogg;codecs=opus", "audio/webm"];
  const mimeType = candidates.find((value) => MediaRecorder.isTypeSupported(value)) || "";
  lumiRecorderChunks = [];
  lumiRecorderStream = stream;
  lumiRecorderStartedAt = Date.now();
  lumiRecorder = mimeType ? new MediaRecorder(stream, { mimeType }) : new MediaRecorder(stream);
  lumiRecorder.ondataavailable = (event) => {
    if (event.data?.size) lumiRecorderChunks.push(event.data);
  };
  lumiRecorder.start(250);
}

export async function stopLumiRecording() {
  if (!lumiRecorder || lumiRecorder.state === "inactive") {
    throw new Error("recorder_not_started");
  }
  const recorder = lumiRecorder;
  const stream = lumiRecorderStream;
  const result = await new Promise((resolve, reject) => {
    recorder.onerror = () => reject(new Error("recording_failed"));
    recorder.onstop = async () => {
      try {
        const blob = new Blob(lumiRecorderChunks, { type: recorder.mimeType || "audio/webm" });
        const bytes = new Uint8Array(await blob.arrayBuffer());
        resolve({
          bytes,
          mediaType: (blob.type || "audio/webm").split(";")[0],
          durationMs: Math.max(1, Date.now() - lumiRecorderStartedAt),
        });
      } catch (error) {
        reject(error);
      }
    };
    recorder.stop();
  });
  stream?.getTracks().forEach((track) => track.stop());
  lumiRecorder = null;
  lumiRecorderStream = null;
  lumiRecorderChunks = [];
  lumiRecorderStartedAt = 0;
  return result;
}

export function cancelLumiRecording() {
  if (lumiRecorder && lumiRecorder.state !== "inactive") {
    lumiRecorder.ondataavailable = null;
    lumiRecorder.onstop = null;
    lumiRecorder.stop();
  }
  lumiRecorderStream?.getTracks().forEach((track) => track.stop());
  lumiRecorder = null;
  lumiRecorderStream = null;
  lumiRecorderChunks = [];
  lumiRecorderStartedAt = 0;
}

export function createLumiAudioUrl(bytes, mediaType) {
  return URL.createObjectURL(new Blob([bytes], { type: mediaType }));
}

export function revokeLumiAudioUrl(url) {
  if (url) URL.revokeObjectURL(url);
}

export async function sleepLumi(milliseconds) {
  await new Promise((resolve) => setTimeout(resolve, milliseconds));
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = startLumiRecording)]
    async fn start_lumi_recording() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = stopLumiRecording)]
    async fn stop_lumi_recording() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = cancelLumiRecording)]
    fn cancel_lumi_recording();
    #[wasm_bindgen(js_name = createLumiAudioUrl)]
    fn create_lumi_audio_url(bytes: &js_sys::Uint8Array, media_type: &str) -> String;
    #[wasm_bindgen(js_name = revokeLumiAudioUrl)]
    fn revoke_lumi_audio_url(url: &str);
    #[wasm_bindgen(js_name = sleepLumi)]
    async fn sleep_lumi(milliseconds: u32);
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn begin_recording() -> Result<(), String> {
    start_lumi_recording()
        .await
        .map(|_| ())
        .map_err(|_| "Браузер не дал доступ к микрофону или не поддерживает запись.".to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn begin_recording() -> Result<(), String> {
    Err("Запись голоса доступна только в Web-сборке.".to_owned())
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn finish_recording() -> Result<RecordedAudio, String> {
    let value = stop_lumi_recording()
        .await
        .map_err(|_| "Не удалось завершить запись.".to_owned())?;
    let bytes = js_sys::Reflect::get(&value, &JsValue::from_str("bytes"))
        .map_err(|_| "Браузер вернул некорректную запись.".to_owned())?;
    let media_type = js_sys::Reflect::get(&value, &JsValue::from_str("mediaType"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "audio/webm".to_owned());
    let duration_ms = js_sys::Reflect::get(&value, &JsValue::from_str("durationMs"))
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.min(600_000.0) as u64);
    let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
    if bytes.is_empty() || bytes.len() > lumi_core::MAX_LEARNING_AUDIO_BYTES as usize {
        return Err("Запись пуста или превышает 25 МиБ.".to_owned());
    }
    Ok(RecordedAudio {
        bytes,
        media_type,
        duration_ms,
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn finish_recording() -> Result<RecordedAudio, String> {
    Err("Запись голоса доступна только в Web-сборке.".to_owned())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn preview_url(recording: &RecordedAudio) -> String {
    let bytes = js_sys::Uint8Array::from(recording.bytes.as_slice());
    create_lumi_audio_url(&bytes, &recording.media_type)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn preview_url(_recording: &RecordedAudio) -> String {
    String::new()
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn revoke_preview(url: &str) {
    revoke_lumi_audio_url(url);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn revoke_preview(_url: &str) {}

#[cfg(target_arch = "wasm32")]
pub(crate) fn cancel_recording() {
    cancel_lumi_recording();
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn cancel_recording() {}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn sleep_one_second() {
    sleep_lumi(1_000).await;
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn sleep_one_second() {}

fn media_type_for_file_name(name: &str) -> Option<&'static str> {
    let lowercase = name.to_ascii_lowercase();
    if lowercase.ends_with(".webm") {
        Some("audio/webm")
    } else if lowercase.ends_with(".ogg") || lowercase.ends_with(".oga") {
        Some("audio/ogg")
    } else if lowercase.ends_with(".m4a") || lowercase.ends_with(".mp4") {
        Some("audio/mp4")
    } else if lowercase.ends_with(".mp3") {
        Some("audio/mpeg")
    } else if lowercase.ends_with(".wav") {
        Some("audio/wav")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_fallback_accepts_only_server_media_allowlist() {
        assert_eq!(media_type_for_file_name("note.M4A"), Some("audio/mp4"));
        assert_eq!(media_type_for_file_name("note.exe"), None);
    }
}
