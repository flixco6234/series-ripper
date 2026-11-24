//! Push notifications when a disc is done, needs review or failed:
//! Discord webhook, ntfy topic URL, Gotify (URL with ?token=...) or a
//! generic JSON POST (Home Assistant, n8n, ...).

use crate::app::App;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>() + "…"
    }
}

pub fn deliver(format: &str, url: &str, kind: &str, title: &str, message: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("no notification URL set".into());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("series-ripper/", env!("CARGO_PKG_VERSION")))
        .build();
    let result = match format {
        "discord" => agent.post(url).send_json(json!({ "content": truncate(&format!("**{title}**\n{message}"), 1900) })),
        "ntfy" => {
            // HTTP header values must be plain ASCII.
            let ascii_title: String = title.chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).collect();
            let tag = match kind {
                "done" => "white_check_mark",
                "review" => "eyes",
                "error" => "warning",
                _ => "cd",
            };
            agent.post(url).set("Title", &ascii_title).set("Tags", tag).send_string(message)
        }
        "gotify" => agent.post(url).send_json(json!({ "title": title, "message": message, "priority": 5 })),
        _ => agent.post(url).send_json(json!({ "event": kind, "title": title, "message": message })),
    };
    result.map(|_| ()).map_err(|e| e.to_string())
}

/// Fire-and-forget notification for event `kind` ("done", "review", "error"),
/// if the user switched that kind on.
pub fn send(app: &Arc<App>, kind: &str, title: &str, message: &str) {
    let cfg = app.cfg();
    let wanted = match kind {
        "done" => cfg.notify_on_done,
        "review" => cfg.notify_on_review,
        _ => cfg.notify_on_error,
    };
    if !wanted || cfg.notify_format == "off" || cfg.notify_url.is_empty() {
        return;
    }
    let (app, kind, title, message) = (app.clone(), kind.to_string(), title.to_string(), message.to_string());
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            deliver(&cfg.notify_format, &cfg.notify_url, &kind, &title, &message)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = result {
            app.log("warn", format!("Notification failed: {e}"));
        }
    });
}

pub async fn test(app: &Arc<App>) -> Result<(), String> {
    let cfg = app.cfg();
    if cfg.notify_format == "off" {
        return Err("Notifications are off - pick a service first.".into());
    }
    tokio::task::spawn_blocking(move || {
        deliver(
            &cfg.notify_format,
            &cfg.notify_url,
            "test",
            "Series Ripper test",
            "Notifications work. You'll get one when a disc is done, needs review or fails.",
        )
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
}
