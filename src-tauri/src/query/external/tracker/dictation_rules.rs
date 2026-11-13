//! `sona work` verbs for the lists in Settings › Vocabulary and what sits
//! beside them: vocabulary words, snippets, rewrites, emoji, writing samples,
//! the words heard in meetings, and the rules that pick a mode for an app or a
//! website. Writes go through the same settings change as `sona work
//! setting`, so a running Sona follows them.

use super::preferences::{
    apply, mode_error, read, scope, show, vocabulary_shown, Change, SettingsChange,
};
use super::{now_ms, Tracker, WorkAnswer};
use crate::audio_toolkit::text::vocabulary_spoken_key;
use crate::commands::{persona, snippets, vocabulary};
use crate::meeting::workflow_engine::known_vocabulary;
use crate::modes::{self, WebsiteHostMatch};
use crate::query::external::{
    kept_local, store, ExternalError, ExternalScope, READ_SCOPES, WRITE_SCOPES,
};
use crate::query::QueryError;
use crate::settings::{
    self, AppSettings, EmojiReplacement, PersonaSample, ReplacementRule, VocabularyEntry,
    PERSONA_SAMPLES_MAX,
};
use crate::snippets::Snippet;
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum DictationRulesVerb {
    /// Settings › Vocabulary: the vocabulary words, snippets, rewrites and
    /// emoji, with their switches.
    Vocabulary(VocabularyArgs),
    /// Take a word out of the vocabulary, by what dictation hears.
    VocabularyRemove(VocabularyRemoveArgs),
    /// Add a snippet: a trigger said in dictation and the text it becomes.
    /// --id changes one.
    SnippetSave(SnippetSaveArgs),
    /// Turn a snippet on, or off with --off.
    SnippetEnable(SnippetEnableArgs),
    /// Delete a snippet.
    SnippetDelete(SnippetDeleteArgs),
    /// Add a rewrite: a phrase dictation hears and what to write instead. A
    /// phrase that has one takes the new text.
    RewriteSave(RewriteSaveArgs),
    /// Turn a rewrite on, or off with --off.
    RewriteEnable(RewriteEnableArgs),
    /// Delete a rewrite, by its phrase.
    RewriteDelete(PhraseArgs),
    /// Add an emoji replacement: a phrase and the emoji written for it. A
    /// phrase that has one takes the new emoji.
    EmojiSave(EmojiSaveArgs),
    /// Delete an emoji replacement, by its phrase.
    EmojiDelete(PhraseArgs),
    /// Add a writing sample cleanup matches your voice to, or change one
    /// with --id.
    PersonaSampleSave(PersonaSampleSaveArgs),
    /// Delete a writing sample.
    PersonaSampleDelete(PersonaSampleDeleteArgs),
    /// Words heard again and again in meetings that the vocabulary lacks,
    /// with how often. Meetings kept on this Mac are left out.
    HeardWords,
    /// Remove the rule that picks a mode for an app or a website.
    ModeRuleRemove(ModeRuleRemoveArgs),
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VocabularyArgs {
    /// A mode's own vocabulary words. Default: every mode's.
    #[arg(long, value_name = "MODE_ID")]
    pub mode: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct VocabularyRemoveArgs {
    /// What dictation hears.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    /// A mode's own vocabulary. Default: every mode's.
    #[arg(long, value_name = "MODE_ID")]
    pub mode: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SnippetSaveArgs {
    /// What is said.
    #[arg(value_name = "TRIGGER")]
    pub trigger: String,
    /// The text it becomes.
    #[arg(long)]
    pub text: String,
    /// The snippet to change, from `sona work vocabulary`.
    #[arg(long)]
    pub id: Option<String>,
    /// Save it turned off.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SnippetEnableArgs {
    #[arg(value_name = "SNIPPET_ID")]
    pub id: String,
    /// Turn it off.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SnippetDeleteArgs {
    #[arg(value_name = "SNIPPET_ID")]
    pub id: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RewriteSaveArgs {
    /// What dictation hears.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    /// What to write instead.
    #[arg(value_name = "WRITTEN", allow_hyphen_values = true)]
    pub written: String,
    /// Save it turned off.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct RewriteEnableArgs {
    /// The rewrite's phrase.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    /// Turn it off.
    #[arg(long)]
    pub off: bool,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PhraseArgs {
    /// The phrase dictation hears.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct EmojiSaveArgs {
    /// What dictation hears.
    #[arg(value_name = "SPOKEN")]
    pub spoken: String,
    /// The emoji to write.
    #[arg(value_name = "EMOJI")]
    pub emoji: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonaSampleSaveArgs {
    /// A paragraph of your own writing.
    #[arg(value_name = "TEXT", allow_hyphen_values = true)]
    pub text: String,
    /// The sample to change, from `sona work settings`.
    #[arg(long)]
    pub id: Option<String>,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct PersonaSampleDeleteArgs {
    #[arg(value_name = "SAMPLE_ID")]
    pub id: String,
    #[command(flatten)]
    pub change: Change,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum HostMatch {
    /// The host only.
    Exact,
    /// The host and its subdomains.
    Subdomains,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
#[group(required = true, multiple = false)]
pub struct RuleTarget {
    /// The app's bundle id, from `sona work settings` under modes.
    #[arg(long)]
    pub app: Option<String>,
    /// The website's host, from `sona work settings` under modes.
    #[arg(long)]
    pub website: Option<String>,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ModeRuleRemoveArgs {
    /// The mode the rule picks.
    #[arg(value_name = "MODE_ID")]
    pub mode: String,
    #[command(flatten)]
    pub target: RuleTarget,
    /// How the website rule matches. Default: exact.
    #[arg(long = "match", value_enum)]
    pub host_match: Option<HostMatch>,
    #[command(flatten)]
    pub change: Change,
}

impl DictationRulesVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self.change() {
            None
            | Some(Change {
                dry_run: true,
                ..
            }) => READ_SCOPES,
            Some(_) => WRITE_SCOPES,
        }
    }

    const fn change(&self) -> Option<&Change> {
        match self {
            Self::Vocabulary(_) | Self::HeardWords => None,
            Self::VocabularyRemove(VocabularyRemoveArgs { change, .. })
            | Self::SnippetSave(SnippetSaveArgs { change, .. })
            | Self::SnippetEnable(SnippetEnableArgs { change, .. })
            | Self::SnippetDelete(SnippetDeleteArgs { change, .. })
            | Self::RewriteSave(RewriteSaveArgs { change, .. })
            | Self::RewriteEnable(RewriteEnableArgs { change, .. })
            | Self::RewriteDelete(PhraseArgs { change, .. })
            | Self::EmojiSave(EmojiSaveArgs { change, .. })
            | Self::EmojiDelete(PhraseArgs { change, .. })
            | Self::PersonaSampleSave(PersonaSampleSaveArgs { change, .. })
            | Self::PersonaSampleDelete(PersonaSampleDeleteArgs { change, .. })
            | Self::ModeRuleRemove(ModeRuleRemoveArgs { change, .. }) => Some(change),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Vocabulary(_) => "vocabulary",
            Self::VocabularyRemove(_) => "vocabulary-remove",
            Self::SnippetSave(_) => "snippet-save",
            Self::SnippetEnable(_) => "snippet-enable",
            Self::SnippetDelete(_) => "snippet-delete",
            Self::RewriteSave(_) => "rewrite-save",
            Self::RewriteEnable(_) => "rewrite-enable",
            Self::RewriteDelete(_) => "rewrite-delete",
            Self::EmojiSave(_) => "emoji-save",
            Self::EmojiDelete(_) => "emoji-delete",
            Self::PersonaSampleSave(_) => "persona-sample-save",
            Self::PersonaSampleDelete(_) => "persona-sample-delete",
            Self::HeardWords => "heard-words",
            Self::ModeRuleRemove(_) => "mode-rule-remove",
        }
    }
}

#[derive(Serialize)]
struct VocabularyView<'a> {
    words: Vec<VocabularyEntry>,
    snippets_on: bool,
    snippets: &'a [Snippet],
    rewrites_on: bool,
    rewrites: &'a [ReplacementRule],
    emoji_on: bool,
    emoji: &'a [EmojiReplacement],
    spoken_edits: bool,
}

/// Rewrites and emoji match what is heard without regard to case or the
/// spaces around it, so one phrase names one entry.
fn same_phrase(saved: &str, asked: &str) -> bool {
    saved.trim().to_lowercase() == asked.trim().to_lowercase()
}

fn remove_vocabulary_word(
    s: &mut AppSettings,
    scope: &vocabulary::VocabularyScope,
    spoken: &str,
) -> Result<(), String> {
    let key = vocabulary_spoken_key(spoken);
    let entries = vocabulary::entries_in(s, scope)?;
    let kept: Vec<VocabularyEntry> = entries
        .iter()
        .filter(|entry| vocabulary_spoken_key(&entry.spoken) != key)
        .cloned()
        .collect();
    if kept.len() == entries.len() {
        return Err(format!("No vocabulary word {}.", spoken.trim()));
    }
    vocabulary::set_entries_in(s, scope, kept).map(|_| ())
}

fn save_rewrite(s: &mut AppSettings, args: &RewriteSaveArgs) -> Result<(), String> {
    let rule = ReplacementRule {
        spoken: args.spoken.clone(),
        written: args.written.clone(),
        enabled: !args.off,
    }
    .trim_outer_whitespace();
    if !rule.is_usable() {
        return Err("A rewrite needs what is heard and what to write.".to_owned());
    }
    let mut rules = s.replacements_rules.clone();
    match rules.iter_mut().find(|saved| same_phrase(&saved.spoken, &args.spoken)) {
        Some(saved) => *saved = rule,
        None => rules.push(rule),
    }
    let saved = vocabulary::save_text_replacements_in(s, rules);
    if !saved.iter().any(|saved| same_phrase(&saved.spoken, &args.spoken)) {
        return Err("Sona keeps no more rewrites. Delete one first.".to_owned());
    }
    Ok(())
}

fn enable_rewrite(s: &mut AppSettings, spoken: &str, enabled: bool) -> Result<(), String> {
    let mut rules = s.replacements_rules.clone();
    let rule = rules
        .iter_mut()
        .find(|rule| same_phrase(&rule.spoken, spoken))
        .ok_or_else(|| format!("No rewrite for {}.", spoken.trim()))?;
    rule.enabled = enabled;
    vocabulary::save_text_replacements_in(s, rules);
    Ok(())
}

fn delete_rewrite(s: &mut AppSettings, spoken: &str) -> Result<(), String> {
    let mut rules = s.replacements_rules.clone();
    let before = rules.len();
    rules.retain(|rule| !same_phrase(&rule.spoken, spoken));
    if rules.len() == before {
        return Err(format!("No rewrite for {}.", spoken.trim()));
    }
    vocabulary::save_text_replacements_in(s, rules);
    Ok(())
}

fn save_emoji(s: &mut AppSettings, spoken: &str, emoji: &str) -> Result<(), String> {
    let replacement = EmojiReplacement {
        spoken: spoken.to_owned(),
        written: emoji.to_owned(),
    };
    let mut list = s.emoji_replacements.clone();
    match list.iter_mut().find(|saved| same_phrase(&saved.spoken, spoken)) {
        Some(saved) => *saved = replacement,
        None => list.push(replacement),
    }
    vocabulary::set_emoji_replacements_in(s, list).map(|_| ())
}

fn delete_emoji(s: &mut AppSettings, spoken: &str) -> Result<(), String> {
    let mut list = s.emoji_replacements.clone();
    let before = list.len();
    list.retain(|saved| !same_phrase(&saved.spoken, spoken));
    if list.len() == before {
        return Err(format!("No emoji replacement for {}.", spoken.trim()));
    }
    vocabulary::set_emoji_replacements_in(s, list).map(|_| ())
}

fn save_persona_sample(
    s: &mut AppSettings,
    id: Option<&str>,
    new_id: &str,
    text: &str,
) -> Result<(), String> {
    let sample = PersonaSample {
        id: id.unwrap_or(new_id).to_owned(),
        text: text.to_owned(),
    };
    if sample.normalized().is_none() {
        return Err("A writing sample needs text.".to_owned());
    }
    let mut samples = s.persona_samples.clone();
    match id {
        Some(id) => {
            let saved = samples
                .iter_mut()
                .find(|saved| saved.id == id)
                .ok_or_else(|| format!("No writing sample {id}."))?;
            *saved = sample;
        }
        None if samples.len() >= PERSONA_SAMPLES_MAX => {
            return Err(format!(
                "Sona keeps {PERSONA_SAMPLES_MAX} writing samples. Delete one first."
            ));
        }
        None => samples.push(sample),
    }
    persona::save_persona_samples_in(s, samples);
    Ok(())
}

fn delete_persona_sample(s: &mut AppSettings, id: &str) -> Result<(), String> {
    let mut samples = s.persona_samples.clone();
    let before = samples.len();
    samples.retain(|saved| saved.id != id);
    if samples.len() == before {
        return Err(format!("No writing sample {id}."));
    }
    persona::save_persona_samples_in(s, samples);
    Ok(())
}

fn remove_mode_rule(s: &mut AppSettings, args: &ModeRuleRemoveArgs) -> Result<(), String> {
    let revision = s.modes_revision;
    if let Some(app) = &args.target.app {
        if args.host_match.is_some() {
            return Err("--match is for a website rule, not an app.".to_owned());
        }
        if !s
            .mode_activation_rules
            .iter()
            .any(|rule| rule.app_id == *app && rule.mode_id == args.mode)
        {
            return Err(format!("No rule picks mode {} for the app {app}.", args.mode));
        }
        return modes::apply_remove_mode_activation_rule(s, app, revision).map_err(mode_error);
    }
    let asked = args.target.website.as_deref().unwrap_or_default();
    let host = crate::context::normalize_website_host(asked)
        .ok_or_else(|| format!("{asked} is not a website host, such as example.com."))?;
    let match_kind = match args.host_match.unwrap_or(HostMatch::Exact) {
        HostMatch::Exact => WebsiteHostMatch::Exact,
        HostMatch::Subdomains => WebsiteHostMatch::Suffix,
    };
    if !s.mode_website_activation_rules.iter().any(|rule| {
        rule.host == host && rule.match_kind == match_kind && rule.mode_id == args.mode
    }) {
        return Err(format!("No rule picks mode {} for the website {host}.", args.mode));
    }
    modes::apply_remove_mode_website_activation_rule(s, &host, match_kind, revision)
        .map_err(mode_error)
}

impl Tracker<'_> {
    pub(super) async fn dictation_rules(
        &self,
        verb: &DictationRulesVerb,
    ) -> Result<WorkAnswer, ExternalError> {
        let what = verb.name().to_owned();
        let (change, plan) = match verb {
            DictationRulesVerb::Vocabulary(args) => {
                let settings = settings::get_settings(self.app);
                let words = vocabulary::entries_in(&settings, &scope(args.mode.as_deref()))
                    .map_err(ExternalError::invalid)?;
                let view = VocabularyView {
                    words,
                    snippets_on: settings.snippets_enabled,
                    snippets: &settings.snippets,
                    rewrites_on: settings.replacements_enabled,
                    rewrites: &settings.replacements_rules,
                    emoji_on: settings.emoji_replacements_enabled,
                    emoji: &settings.emoji_replacements,
                    spoken_edits: settings.spoken_edits_enabled,
                };
                return Ok(read(&what, &view));
            }
            DictationRulesVerb::HeardWords => {
                let store = store(self.meetings).await?;
                let hidden = kept_local(&store)?;
                let heard = store
                    .vocabulary_candidates_without(&known_vocabulary(Some(self.app)), hidden.sessions())
                    .map_err(|error| ExternalError::from(QueryError::from(error)))?;
                return Ok(read(&what, &heard.entries));
            }
            DictationRulesVerb::VocabularyRemove(args) => {
                let scope = scope(args.mode.as_deref());
                (
                    &args.change,
                    SettingsChange {
                        what,
                        request: json!({ "spoken": args.spoken, "mode": args.mode }),
                        show: vocabulary_shown(scope.clone()),
                        apply: apply(move |s| remove_vocabulary_word(s, &scope, &args.spoken)),
                    },
                )
            }
            DictationRulesVerb::SnippetSave(args) => {
                let now = now_ms();
                (
                    &args.change,
                    SettingsChange {
                        what,
                        request: json!({ "trigger": args.trigger, "text": args.text, "id": args.id, "enabled": !args.off }),
                        show: show(|s| json!(s.snippets)),
                        apply: apply(move |s| {
                            if let Some(id) = &args.id {
                                if !s.snippets.iter().any(|snippet| snippet.id == *id) {
                                    return Err(format!("No snippet {id}."));
                                }
                            }
                            let snippet = Snippet {
                                id: args.id.clone().unwrap_or_default(),
                                trigger: args.trigger.clone(),
                                expansion: args.text.clone(),
                                enabled: !args.off,
                                created_at: now,
                                updated_at: now,
                            };
                            snippets::upsert_snippet_in(s, snippet, now)
                        }),
                    },
                )
            }
            DictationRulesVerb::SnippetEnable(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "id": args.id, "enabled": !args.off }),
                    show: show(|s| json!(s.snippets)),
                    apply: apply(|s| snippets::set_snippet_enabled_in(s, &args.id, !args.off, now_ms())),
                },
            ),
            DictationRulesVerb::SnippetDelete(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "id": args.id }),
                    show: show(|s| json!(s.snippets)),
                    apply: apply(|s| snippets::delete_snippet_in(s, &args.id)),
                },
            ),
            DictationRulesVerb::RewriteSave(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "spoken": args.spoken, "written": args.written, "enabled": !args.off }),
                    show: show(|s| json!(s.replacements_rules)),
                    apply: apply(|s| save_rewrite(s, args)),
                },
            ),
            DictationRulesVerb::RewriteEnable(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "spoken": args.spoken, "enabled": !args.off }),
                    show: show(|s| json!(s.replacements_rules)),
                    apply: apply(|s| enable_rewrite(s, &args.spoken, !args.off)),
                },
            ),
            DictationRulesVerb::RewriteDelete(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "spoken": args.spoken }),
                    show: show(|s| json!(s.replacements_rules)),
                    apply: apply(|s| delete_rewrite(s, &args.spoken)),
                },
            ),
            DictationRulesVerb::EmojiSave(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "spoken": args.spoken, "emoji": args.emoji }),
                    show: show(|s| json!(s.emoji_replacements)),
                    apply: apply(|s| save_emoji(s, &args.spoken, &args.emoji)),
                },
            ),
            DictationRulesVerb::EmojiDelete(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "spoken": args.spoken }),
                    show: show(|s| json!(s.emoji_replacements)),
                    apply: apply(|s| delete_emoji(s, &args.spoken)),
                },
            ),
            DictationRulesVerb::PersonaSampleSave(args) => {
                let new_id = format!("sample_{}", now_ms());
                (
                    &args.change,
                    SettingsChange {
                        what,
                        request: json!({ "id": args.id, "text": args.text }),
                        show: show(|s| json!(s.persona_samples)),
                        apply: apply(move |s| {
                            save_persona_sample(s, args.id.as_deref(), &new_id, &args.text)
                        }),
                    },
                )
            }
            DictationRulesVerb::PersonaSampleDelete(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({ "id": args.id }),
                    show: show(|s| json!(s.persona_samples)),
                    apply: apply(|s| delete_persona_sample(s, &args.id)),
                },
            ),
            DictationRulesVerb::ModeRuleRemove(args) => (
                &args.change,
                SettingsChange {
                    what,
                    request: json!({
                        "mode": args.mode,
                        "app": args.target.app,
                        "website": args.target.website,
                        "match": args.host_match,
                    }),
                    show: show(|s| {
                        json!({
                            "mode_activation_rules": s.mode_activation_rules,
                            "mode_website_activation_rules": s.mode_website_activation_rules,
                        })
                    }),
                    apply: apply(|s| remove_mode_rule(s, args)),
                },
            ),
        };
        self.change_settings(change, plan, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removal(mode: &str, app: &str) -> ModeRuleRemoveArgs {
        ModeRuleRemoveArgs {
            mode: mode.to_owned(),
            target: RuleTarget {
                app: Some(app.to_owned()),
                website: None,
            },
            host_match: None,
            change: Change {
                dry_run: false,
                write: super::super::WriteArgs {
                    operation_id: None,
                    agent: None,
                },
            },
        }
    }

    #[test]
    fn a_rule_is_removed_only_for_the_mode_it_picks() {
        let mut s = settings::get_default_settings();
        s.mode_activation_rules = vec![modes::ModeActivationRule {
            app_id: "com.apple.mail".to_owned(),
            mode_id: "email".to_owned(),
        }];
        assert!(remove_mode_rule(&mut s, &removal("notes", "com.apple.mail")).is_err());
        assert_eq!(s.mode_activation_rules.len(), 1);
        remove_mode_rule(&mut s, &removal("email", "com.apple.mail")).unwrap();
        assert!(s.mode_activation_rules.is_empty());
    }

    #[test]
    fn a_rewrite_saved_again_replaces_the_one_heard_the_same() {
        let mut s = settings::get_default_settings();
        s.replacements_rules.clear();
        let save = |spoken: &str, written: &str| RewriteSaveArgs {
            spoken: spoken.to_owned(),
            written: written.to_owned(),
            off: false,
            change: removal("", "").change,
        };
        save_rewrite(&mut s, &save("sona dot app", "sona.app")).unwrap();
        save_rewrite(&mut s, &save(" Sona Dot App", "Sona.app")).unwrap();
        assert_eq!(s.replacements_rules.len(), 1);
        assert_eq!(s.replacements_rules[0].written, "Sona.app");
    }
}
