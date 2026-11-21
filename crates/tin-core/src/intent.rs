//! Why something was saved: what the user said it's for, and where that routes it.
//!
//! The user types or says a purpose ("for the blog", "game dev", "new project: pixel farm").
//! Tin matches it against routes from the config and the user's project folders, so a capture
//! lands tagged for an existing project (and which one), a new project, or a topic.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{Config, expand_home};

/// The purpose of a capture, resolved against the known routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Intent {
    /// What the user typed or said it's for, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    pub target: Target,
    /// Lowercase tags: matched route names, `#hashtags`, and `new-project`.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Where the capture is headed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Target {
    /// Nothing named a project; an agent (or the user) decides later.
    #[default]
    Undecided,
    /// An existing project.
    Project {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// The words that matched, as said.
        matched: String,
    },
    /// A project that doesn't exist yet.
    NewProject {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl Target {
    /// The project's or new project's name, for display and filtering.
    pub fn name(&self) -> Option<&str> {
        match self {
            Target::Undecided => None,
            Target::Project { name, .. } => Some(name),
            Target::NewProject { name } => name.as_deref(),
        }
    }
}

/// Something a purpose can name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub name: String,
    pub kind: RouteKind,
    /// Each way to say it, as lowercase words.
    phrases: Vec<Vec<String>>,
    pub hook: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteKind {
    /// Configured with a folder, or found in a project root.
    Project { path: PathBuf, discovered: bool },
    /// Configured without a folder: a tag only.
    Topic,
}

/// Every route a purpose can match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Routes(Vec<Route>);

/// Words that introduce a project folder's name ("for tin", "ask tin"). Folder names are often
/// ordinary words ("notes", "site"), so a folder found on disk matches only after one of these,
/// or as the whole purpose. Configured routes match anywhere.
const CUES: [&str; 12] = ["for", "to", "in", "into", "on", "ask", "project", "re", "about", "with", "add", "tell"];

impl Routes {
    /// Configured routes first, then project folders that no route already names.
    pub fn from_config(config: &Config) -> Routes {
        let mut discovered: Vec<(String, PathBuf)> = config
            .project_roots()
            .iter()
            .flat_map(|root| std::fs::read_dir(root).into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter_map(|p| {
                let name = p.file_name()?.to_str()?.to_owned();
                (!name.starts_with('.')).then_some((name, p))
            })
            .collect();
        discovered.sort();
        let configured = config.routes.iter().map(|r| {
            let folder = discovered.iter().find(|(n, _)| words(n) == words(&r.name)).map(|(_, p)| p.clone());
            let kind = match (&r.path, folder) {
                (Some(path), _) => RouteKind::Project { path: expand_home(path), discovered: false },
                (None, Some(path)) => RouteKind::Project { path, discovered: false },
                (None, None) => RouteKind::Topic,
            };
            Route::new(&r.name, &r.aliases, kind, r.hook.clone())
        });
        let mut routes: Vec<Route> = configured.collect();
        for (name, path) in discovered {
            if !routes.iter().any(|r| words(&r.name) == words(&name)) {
                routes.push(Route::new(&name, &[], RouteKind::Project { path, discovered: true }, None));
            }
        }
        Routes(routes)
    }

    pub fn new(routes: Vec<Route>) -> Routes {
        Routes(routes)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Route> {
        self.0.iter()
    }

    pub fn get(&self, name: &str) -> Option<&Route> {
        self.0.iter().find(|r| r.name == name)
    }

    /// Resolves what the user said a capture is for. `extra` is more of their words that may
    /// name a route (the voice note), searched after `said`.
    pub fn resolve(&self, said: Option<&str>, extra: Option<&str>) -> Intent {
        let said = said.map(str::trim).filter(|s| !s.is_empty());
        let texts: Vec<&str> = said.into_iter().chain(extra.map(str::trim).filter(|s| !s.is_empty())).collect();
        let mut tags: Vec<String> = Vec::new();
        let mut project: Option<(&Route, String)> = None;
        let mut new_project: Option<Option<String>> = None;
        for text in &texts {
            let tokens = words(text);
            if new_project.is_none() {
                new_project = new_project_name(&tokens);
            }
            let mut hits: Vec<(usize, usize, &Route)> =
                self.0.iter().filter_map(|route| route.find(&tokens).map(|(at, len)| (at, len, route))).collect();
            hits.sort_by_key(|(at, len, _)| (*at, std::cmp::Reverse(*len)));
            for (at, len, route) in hits {
                push_tag(&mut tags, &route.name);
                if project.is_none() && matches!(route.kind, RouteKind::Project { .. }) {
                    project = Some((route, tokens[at..at + len].join(" ")));
                }
            }
            for word in text.split_whitespace() {
                if let Some(tag) = word.strip_prefix('#') {
                    let tag = tag.trim_end_matches(|c: char| !c.is_alphanumeric());
                    if !tag.is_empty() {
                        push_tag(&mut tags, tag);
                    }
                }
            }
        }
        let target = match (new_project, project) {
            (Some(name), _) => {
                push_tag(&mut tags, "new-project");
                Target::NewProject { name }
            }
            (None, Some((route, matched))) => Target::Project {
                name: route.name.clone(),
                path: match &route.kind {
                    RouteKind::Project { path, .. } => Some(path.clone()),
                    RouteKind::Topic => None,
                },
                matched,
            },
            (None, None) => Target::Undecided,
        };
        Intent { said: said.map(str::to_owned), target, tags }
    }
}

impl Route {
    pub fn new(name: &str, aliases: &[String], kind: RouteKind, hook: Option<Vec<String>>) -> Route {
        let phrases = std::iter::once(name)
            .chain(aliases.iter().map(String::as_str))
            .map(words)
            .filter(|w| !w.is_empty())
            .collect();
        Route { name: name.to_owned(), kind, phrases, hook }
    }

    /// Where (word index) and how long the best phrase match in `tokens` is.
    fn find(&self, tokens: &[String]) -> Option<(usize, usize)> {
        let needs_cue = matches!(self.kind, RouteKind::Project { discovered: true, .. });
        self.phrases
            .iter()
            .flat_map(|phrase| {
                (0..tokens.len().saturating_sub(phrase.len() - 1))
                    .filter(move |&at| tokens[at..at + phrase.len()] == phrase[..])
                    .map(move |at| (at, phrase.len()))
            })
            .filter(|&(at, len)| {
                let whole = at == 0 && len == tokens.len();
                let cued = at > 0 && CUES.contains(&tokens[at - 1].as_str());
                !needs_cue || whole || cued
            })
            .min_by_key(|&(at, len)| (at, std::cmp::Reverse(len)))
    }
}

/// `new project`, `new project: pixel farm`, `a new project called pixel farm`.
fn new_project_name(tokens: &[String]) -> Option<Option<String>> {
    let at = tokens.windows(2).position(|w| w[0] == "new" && matches!(w[1].as_str(), "project" | "app" | "idea"))?;
    let rest: Vec<&str> = tokens[at + 2..]
        .iter()
        .map(String::as_str)
        .skip_while(|w| matches!(*w, "called" | "named" | "for" | "about"))
        .collect();
    Some((!rest.is_empty()).then(|| rest.join(" ")))
}

/// Lowercase alphanumeric words: `See Example.com!` is `see example com`.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

fn push_tag(tags: &mut Vec<String>, tag: &str) {
    let tag = crate::tag_of(tag);
    if !tag.is_empty() && !tags.contains(&tag) {
        tags.push(tag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes() -> Routes {
        let project = |p: &str| RouteKind::Project { path: PathBuf::from(p), discovered: false };
        let found = |p: &str| RouteKind::Project { path: PathBuf::from(p), discovered: true };
        Routes::new(vec![
            Route::new("blog", &["for the blog".into(), "example.com".into()], project("/dev/blog"), None),
            Route::new("game dev", &[], RouteKind::Topic, None),
            Route::new("tin", &[], found("/dev/tin"), None),
            Route::new("notes", &[], found("/dev/notes"), None),
        ])
    }

    #[test]
    fn an_alias_routes_to_its_project() {
        let intent = routes().resolve(Some("Save this For the Blog"), None);
        assert_eq!(
            intent.target,
            Target::Project { name: "blog".into(), path: Some("/dev/blog".into()), matched: "for the blog".into() }
        );
        assert_eq!(intent.tags, ["blog"]);
        assert_eq!(intent.said.as_deref(), Some("Save this For the Blog"));
    }

    #[test]
    fn a_domain_routes_too() {
        assert_eq!(routes().resolve(Some("example.com"), None).target.name(), Some("blog"));
    }

    #[test]
    fn a_topic_tags_without_naming_a_project() {
        let intent = routes().resolve(Some("game dev #shaders"), None);
        assert_eq!(intent.target, Target::Undecided);
        assert_eq!(intent.tags, ["game-dev", "shaders"]);
    }

    #[test]
    fn a_discovered_folder_needs_a_cue_or_the_whole_purpose() {
        assert_eq!(routes().resolve(Some("for tin"), None).target.name(), Some("tin"));
        assert_eq!(routes().resolve(Some("tin"), None).target.name(), Some("tin"));
        let casual = routes().resolve(Some("good notes on shaders"), None);
        assert_eq!(casual.target, Target::Undecided);
        assert!(casual.tags.is_empty());
    }

    #[test]
    fn new_project_wins_and_takes_a_name() {
        let intent = routes().resolve(Some("new project called Pixel Farm, like game dev"), None);
        assert_eq!(intent.target, Target::NewProject { name: Some("pixel farm like game dev".into()) });
        assert_eq!(intent.tags, ["game-dev", "new-project"]);
        assert_eq!(routes().resolve(Some("new idea"), None).target, Target::NewProject { name: None });
    }

    #[test]
    fn the_voice_note_routes_when_nothing_was_typed() {
        let intent = routes().resolve(None, Some("This one is for the blog, the post about shaders"));
        assert_eq!(intent.target.name(), Some("blog"));
        assert_eq!(intent.said, None);
    }

    #[test]
    fn the_typed_purpose_beats_the_voice_note() {
        let intent = routes().resolve(Some("for tin"), Some("for the blog"));
        assert_eq!(intent.target.name(), Some("tin"));
        assert_eq!(intent.tags, ["tin", "blog"]);
    }

    #[test]
    fn nothing_said_is_undecided() {
        assert_eq!(routes().resolve(Some("  "), None), Intent::default());
    }
}
