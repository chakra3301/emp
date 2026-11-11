//! `models.list`: what each harness can run, read from the harness itself.
//!
//! - pi: `pi --list-models` (provider/model table), cached for 10 minutes.
//! - Codex: `$CODEX_HOME/models_cache.json` (Codex keeps it fresh).
//! - Claude Code: its documented aliases (`claude --help`); full ids also work but the aliases
//!   follow the latest model.
//! - Hermes: the profile's `provider_models_cache.json`, as `provider:model` ACP ids.
//! - Antigravity: `agy models` (`id<TAB>label` lines), cached for 10 minutes.
//!
//! Context windows come from the same sources where they're listed (pi's table, Codex's
//! cache, Claude's `[1m]` aliases); [`window_for`] fills in the rest for `threads.context`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use emp_protocol::{Harness, ModelOption};

use crate::doctor;

const PI_CACHE: Duration = Duration::from_secs(600);

static PI_MODELS: Mutex<Option<(Instant, Vec<ModelOption>)>> = Mutex::new(None);
static AGY_MODELS: Mutex<Option<(Instant, Vec<ModelOption>)>> = Mutex::new(None);

pub async fn list(harness: Harness, profile: Option<String>) -> Result<Vec<ModelOption>> {
    let models = match harness {
        Harness::Pi => pi().await?,
        Harness::Codex => codex(),
        Harness::Claude => claude(),
        Harness::Hermes => hermes(profile.as_deref()),
        Harness::Antigravity => antigravity().await?,
        _ => Vec::new(),
    };
    Ok(without_copilot(models))
}

/// Copilot is disabled in EMP, including its ACP and GitHub-prefixed provider aliases.
pub fn is_copilot_model(model: &str) -> bool {
    is_copilot_provider(model.split(['/', ':']).next().unwrap_or(model))
}

fn is_copilot_provider(provider: &str) -> bool {
    provider.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part.eq_ignore_ascii_case("copilot"))
}

fn without_copilot(models: Vec<ModelOption>) -> Vec<ModelOption> {
    models.into_iter().filter(|m| !is_copilot_model(&m.id) && !m.group.as_deref().is_some_and(is_copilot_provider)).collect()
}

/// Context window for `model` on `harness`: the harness's own listing first, then what's
/// known about the model family. `None` when unknown.
pub async fn window_for(harness: Harness, model: &str) -> Option<u64> {
    let listed = match harness {
        Harness::Pi | Harness::Codex | Harness::Antigravity => list(harness, None).await.unwrap_or_default(),
        Harness::Claude => claude(),
        _ => Vec::new(),
    };
    let bare = model.rsplit(['/', ':']).next().unwrap_or(model);
    listed
        .iter()
        .find(|m| m.id == model)
        .or_else(|| listed.iter().find(|m| m.id.rsplit(['/', ':']).next() == Some(bare)))
        .and_then(|m| m.context)
        .or_else(|| family_window(bare))
}

/// Windows by family, for harnesses that don't list them.
fn family_window(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") {
        Some(1_000_000)
    } else if m.starts_with("gemini") {
        Some(1_048_576)
    } else if m.starts_with("claude") || matches!(m.as_str(), "opus" | "sonnet" | "haiku" | "fable") {
        Some(200_000)
    } else if m.starts_with("gpt-oss") {
        Some(131_072)
    } else {
        None
    }
}

async fn antigravity() -> Result<Vec<ModelOption>> {
    if let Some((at, list)) = AGY_MODELS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && at.elapsed() < PI_CACHE
    {
        return Ok(list.clone());
    }
    let Some(bin) = doctor::which("agy") else { return Ok(Vec::new()) };
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(bin).arg("models").stdin(std::process::Stdio::null()).kill_on_drop(true).output(),
    )
    .await??;
    let list = parse_agy(&String::from_utf8_lossy(&out.stdout));
    if !list.is_empty() {
        *AGY_MODELS.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), list.clone()));
    }
    Ok(list)
}

/// `id<TAB>label` lines (after a "Fetching…" line).
pub fn parse_agy(text: &str) -> Vec<ModelOption> {
    text.lines()
        .filter_map(|l| {
            let (id, label) = l.split_once('\t')?;
            let id = id.trim();
            let group = if id.starts_with("gemini") {
                "google"
            } else if id.starts_with("claude") {
                "anthropic"
            } else if id.starts_with("gpt") {
                "openai"
            } else {
                "other"
            };
            Some(ModelOption { id: id.into(), label: label.trim().into(), group: Some(group.into()), context: family_window(id) })
        })
        .collect()
}

/// `1M` → 1 000 000, `272K` → 272 000.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'M' | 'm' => (&s[..s.len() - 1], 1_000_000.0),
        'K' | 'k' => (&s[..s.len() - 1], 1_000.0),
        _ => (s, 1.0),
    };
    num.parse::<f64>().ok().map(|n| (n * mult) as u64)
}

async fn pi() -> Result<Vec<ModelOption>> {
    if let Some((at, list)) = PI_MODELS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && at.elapsed() < PI_CACHE
    {
        return Ok(list.clone());
    }
    let Some(bin) = doctor::which("pi") else { return Ok(Vec::new()) };
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(bin).arg("--list-models").kill_on_drop(true).output(),
    )
    .await??;
    let list = parse_pi(&String::from_utf8_lossy(&out.stdout));
    *PI_MODELS.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// Rows of `provider  model  context …` after the header line.
pub fn parse_pi(text: &str) -> Vec<ModelOption> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("provider"))
        .skip(1)
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let (provider, model) = (cols.next()?, cols.next()?);
            Some(ModelOption {
                id: format!("{provider}/{model}"),
                label: model.to_string(),
                group: Some(provider.to_string()),
                context: cols.next().and_then(parse_size),
            })
        })
        .collect()
}

fn codex() -> Vec<ModelOption> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| emp_core::paths::home_dir().join(".codex"));
    let Ok(raw) = std::fs::read(home.join("models_cache.json")) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { return Vec::new() };
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let slug = m["slug"].as_str()?;
            (!slug.contains("auto-review")).then(|| ModelOption {
                id: slug.to_string(),
                label: m["display_name"].as_str().unwrap_or(slug).to_string(),
                group: Some("openai".into()),
                context: m["context_window"].as_u64(),
            })
        })
        .collect()
}

fn claude() -> Vec<ModelOption> {
    [
        ("fable", "Fable (latest)"),
        ("opus", "Opus (latest)"),
        ("opus[1m]", "Opus, 1M context"),
        ("sonnet", "Sonnet (latest)"),
        ("sonnet[1m]", "Sonnet, 1M context"),
        ("haiku", "Haiku (latest)"),
    ]
    .into_iter()
    .map(|(id, label)| ModelOption { id: id.into(), label: label.into(), group: Some("anthropic".into()), context: family_window(id) })
    .collect()
}

fn hermes(profile: Option<&str>) -> Vec<ModelOption> {
    let root = emp_core::paths::home_dir().join(".hermes");
    let dir = match profile.filter(|p| !p.is_empty() && *p != "default") {
        Some(p) => root.join("profiles").join(p),
        None => root,
    };
    let Ok(raw) = std::fs::read(dir.join("provider_models_cache.json")) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { return Vec::new() };
    let rejected = emp_import::hermes::rejected_models(profile.unwrap_or("default"), emp_core::now_ms() - 3_600_000).unwrap_or_default();
    hermes_cached_models(&v).into_iter().filter(|m| !rejected.contains(&m.id)).collect()
}

fn hermes_cached_models(v: &serde_json::Value) -> Vec<ModelOption> {
    let mut out = Vec::new();
    for (provider, entry) in v.as_object().into_iter().flatten() {
        // A curated outage fallback isn't the account's discovered model catalogue.
        if entry["fallback"] == true {
            continue;
        }
        let name = match provider.as_str() {
            "openai-codex" => "ChatGPT",
            "copilot" => "GitHub Copilot",
            "anthropic" => "Anthropic API",
            "claude-subscription-directsdk-experimental" => "Claude subscription (experimental)",
            _ => provider,
        };
        for m in entry["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str())
        {
            out.push(ModelOption {
                id: format!("{provider}:{m}"),
                label: format!("{m} · {name}"),
                group: Some(name.to_string()),
                context: None,
            });
        }
    }
    out.sort_by(|a, b| a.group.cmp(&b.group).then(a.label.cmp(&b.label)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_choices_keep_the_provider_visible_and_skip_fallbacks() {
        let m = without_copilot(hermes_cached_models(&serde_json::json!({
            "copilot": {"models": ["gpt-6.1-sol"]},
            "openai-codex": {"models": ["gpt-6.1-sol"]},
            "anthropic": {"models": ["claude-example"], "fallback": true}
        })));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "openai-codex:gpt-6.1-sol");
        assert_eq!(m[0].label, "gpt-6.1-sol · ChatGPT");
    }

    #[test]
    fn copilot_is_excluded_across_harnesses_and_provider_aliases() {
        for id in ["copilot:gpt-6-astra", "copilot-acp:gpt-6.1-sol", "github-copilot/gpt-6-astra", "GitHub_Copilot/gpt-6-astra"] {
            assert!(is_copilot_model(id), "{id}");
        }
        let m = without_copilot(vec![
            ModelOption { id: "gpt-6.1-sol".into(), label: "GPT Sol".into(), group: Some("GitHub Copilot".into()), context: None },
            ModelOption { id: "openai/gpt-6.1-sol".into(), label: "GPT Sol".into(), group: Some("openai".into()), context: None },
        ]);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "openai/gpt-6.1-sol");
    }

    #[test]
    fn parses_pi_table() {
        let text = "provider       model                       context  max-out\nanthropic      claude-fable-5-1            1M       128K\nopenai-codex   gpt-6.1-sol                 272K     128K\n";
        let m = parse_pi(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].id, "openai-codex/gpt-6.1-sol");
        assert_eq!(m[1].group.as_deref(), Some("openai-codex"));
        assert_eq!((m[0].context, m[1].context), (Some(1_000_000), Some(272_000)));
    }

    #[test]
    fn parses_agy_models() {
        let m = parse_agy("Fetching available models...\ngemini-4-argon-medium\tGemini 4 Argon (Medium)\nclaude-opus-5-5-high\tClaude Opus 5.5 (High)\n");
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].id.as_str(), m[0].label.as_str(), m[0].group.as_deref()), ("gemini-4-argon-medium", "Gemini 4 Argon (Medium)", Some("google")));
        assert_eq!(m[1].group.as_deref(), Some("anthropic"));
        assert_eq!(family_window("opus[1m]"), Some(1_000_000));
    }
}
