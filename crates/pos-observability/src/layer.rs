use crate::redact::{REDACTED, is_secret_field, mask_card_numbers};
use serde_json::{Map, Value};
use std::fmt;
use std::io::Write;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Writes one redacted JSON object per event.
pub struct JsonLayer<W> {
    make_writer: W,
}

impl<W> JsonLayer<W> {
    pub fn new(make_writer: W) -> Self {
        Self { make_writer }
    }
}

struct SpanFields(Map<String, Value>);

#[derive(Default)]
struct Fields(Map<String, Value>);

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        let value = if is_secret_field(field.name()) {
            Value::String(REDACTED.to_owned())
        } else {
            value
        };
        self.0.insert(field.name().to_owned(), value);
    }
    fn put_text(&mut self, field: &Field, text: &str) {
        let masked = mask_card_numbers(text).into_owned();
        self.put(field, Value::String(masked));
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put_text(field, value);
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put_text(field, &format!("{value:?}"));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }
    #[allow(clippy::disallowed_types)] // required by the `Visit` trait; never used for money
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(
            field,
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put_text(field, &value.to_string());
    }
}

impl<S, W> Layer<S> for JsonLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(fields.0));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(existing) = ext.get_mut::<SpanFields>() {
                existing.0.extend(fields.0);
            } else {
                ext.insert(SpanFields(fields.0));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut obj = Map::new();
        obj.insert(
            "ts".into(),
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        );
        obj.insert("level".into(), meta.level().as_str().into());
        obj.insert("target".into(), meta.target().into());
        for (k, v) in crate::context_snapshot() {
            obj.insert(k.to_owned(), Value::String(v));
        }
        if let Some(scope) = ctx.event_scope(event) {
            let mut names = Vec::new();
            for span in scope.from_root() {
                names.push(span.name());
                if let Some(f) = span.extensions().get::<SpanFields>() {
                    obj.extend(f.0.clone());
                }
            }
            obj.insert("spans".into(), Value::from(names.join(":")));
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        obj.extend(fields.0);

        let mut line = Value::Object(obj).to_string();
        line.push('\n');
        let mut writer = self.make_writer.make_writer_for(meta);
        let _ = writer.write_all(line.as_bytes());
    }
}

/// Test utility: runs `f` with a subscriber that captures JSON lines, and returns them.
#[doc(hidden)]
pub fn capture_json_lines(f: impl FnOnce()) -> Vec<Value> {
    tests_support::capture(f)
}

mod tests_support {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    pub(crate) fn capture(f: impl FnOnce()) -> Vec<Value> {
        let cap = Capture::default();
        let subscriber = tracing_subscriber::registry().with(JsonLayer::new(cap.clone()));
        tracing::subscriber::with_default(subscriber, f);
        let bytes = cap.0.lock().map(|b| b.clone()).unwrap_or_default();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::capture;
    use super::*;

    #[test]
    fn lines_have_standard_fields_span_context_and_redaction() {
        crate::set_context("register_id", "reg-1");
        let lines = capture(|| {
            let span = tracing::info_span!("ipc", command = "auth_login", correlation_id = "corr-1");
            let _e = span.enter();
            tracing::warn!(pin = "4821", refresh_token = %"tok-secret", user = "alice", note = "paid with 4111 1111 1111 1111", "login {}", "failed");
        });
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        for key in [
            "ts",
            "level",
            "target",
            "message",
            "correlation_id",
            "register_id",
        ] {
            assert!(l.get(key).is_some(), "missing {key}: {l}");
        }
        assert_eq!(l["level"], "WARN");
        assert_eq!(l["message"], "login failed");
        assert_eq!(l["correlation_id"], "corr-1");
        assert_eq!(l["register_id"], "reg-1");
        assert_eq!(l["pin"], REDACTED);
        assert_eq!(l["refresh_token"], REDACTED);
        assert_eq!(l["user"], "alice");
        assert_eq!(l["note"], "paid with [CARD …1111]");
        let raw = l.to_string();
        assert!(
            !raw.contains("4821") && !raw.contains("tok-secret") && !raw.contains("4111 1111"),
            "{raw}"
        );
    }

    #[test]
    fn span_fields_named_like_secrets_are_redacted_too() {
        let lines = capture(|| {
            let span = tracing::info_span!("request", authorization = "Bearer abc.def");
            let _e = span.enter();
            tracing::info!("handled");
        });
        assert_eq!(lines[0]["authorization"], REDACTED);
    }
}
