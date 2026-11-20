//! `tin`: capture from the command line or a desktop shortcut, and let agents read, route and
//! check off what was captured.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use tin_core::capture::{Capture, CapturedText, Draft, Status, TextSource};
use tin_core::context::{self, Options, Part, TextGrab};
use tin_core::intent::{Intent, RouteKind, Target};
use tin_core::page::{Page, PageKind, PageSource};
use tin_core::{Paths, Tin, tag_of};

#[derive(Parser)]
#[command(name = "tin", version, about = "Capture what you're looking at, say what it's for, hand it to an agent.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Capture the selection and the page you're on, with an optional voice note and purpose.
    Capture(CaptureArgs),
    /// Print what a capture would pick up right now, and why anything is missing.
    Context {
        #[arg(long)]
        json: bool,
    },
    /// List captures, newest first (open ones unless --done or --all).
    List(ListArgs),
    /// Print one capture (an id, a unique part of one, or `last`).
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Check captures off.
    Done { ids: Vec<String> },
    /// Open checked-off captures again.
    Reopen { ids: Vec<String> },
    /// Say what a capture is for again, and route it from that.
    Route { id: String, purpose: Vec<String> },
    /// Copy a capture's text (and page URL) to the clipboard.
    Copy { id: String },
    /// Delete a capture.
    Rm { ids: Vec<String> },
    /// List the routes a purpose can name: configured ones and project folders.
    Routes {
        #[arg(long)]
        json: bool,
    },
    /// Write a commented starter config, if there isn't one.
    Init,
    /// Print where Tin keeps its config and captures.
    Paths,
}

#[derive(Args)]
struct CaptureArgs {
    /// What it's for: "ask blog", "for game dev", "new project: pixel farm".
    #[arg(long = "for", short = 'f', value_name = "PURPOSE")]
    purpose: Option<String>,
    /// A typed note to keep with it.
    #[arg(long, short)]
    note: Option<String>,
    /// Record a voice note with Hex; press Enter to stop.
    #[arg(long, short)]
    voice: bool,
    /// Capture this text instead of the selection.
    #[arg(long, conflicts_with_all = ["stdin", "clipboard"])]
    text: Option<String>,
    /// Capture text piped on stdin instead of the selection.
    #[arg(long, conflicts_with_all = ["clipboard", "voice"])]
    stdin: bool,
    /// Capture the clipboard instead of the selection.
    #[arg(long)]
    clipboard: bool,
    /// Don't capture any text, only the page and notes.
    #[arg(long, conflicts_with_all = ["text", "stdin", "clipboard"])]
    no_text: bool,
    /// The page this is about, instead of the browser's active tab.
    #[arg(long, conflicts_with = "no_page")]
    url: Option<String>,
    /// Don't attach the browser's active tab.
    #[arg(long)]
    no_page: bool,
    /// Attach a picture of the focused window when it isn't a browser (COSMIC for now).
    #[arg(long)]
    screenshot: bool,
    /// Print the saved capture as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ListArgs {
    /// Only captures routed or tagged to this project, topic or tag.
    #[arg(long)]
    route: Option<String>,
    /// Only checked-off captures.
    #[arg(long, conflicts_with = "all")]
    done: bool,
    /// Open and checked-off captures.
    #[arg(long)]
    all: bool,
    #[arg(long, short = 'n')]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tin: {message}");
            ExitCode::FAILURE
        }
    }
}

type Outcome = Result<(), String>;

fn open() -> Result<Tin, String> {
    Tin::open(Paths::from_env()).map_err(|e| e.to_string())
}

fn run(command: Command) -> Outcome {
    match command {
        Command::Capture(args) => capture(args),
        Command::Context { json } => show_context(json),
        Command::List(args) => list(args),
        Command::Show { id, json } => {
            let tin = open()?;
            let capture =
                tin.store.load(&tin.store.resolve(&id).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            if json {
                print_json(&capture)
            } else {
                print_capture(&capture, &tin.store.path(&capture.id).display().to_string())
            }
            Ok(())
        }
        Command::Done { ids } => set_done(&ids, true),
        Command::Reopen { ids } => set_done(&ids, false),
        Command::Route { id, purpose } => {
            let tin = open()?;
            let purpose = purpose.join(" ");
            if purpose.trim().is_empty() {
                return Err("say what it's for, e.g. `tin route last ask blog`".into());
            }
            let id = tin.store.resolve(&id).map_err(|e| e.to_string())?;
            let saved = tin.reroute(&id, &purpose).map_err(|e| e.to_string())?;
            warn_all(saved.hook_errors.iter().map(ToString::to_string));
            println!("{} → {}", short(&saved.capture), describe_intent(&saved.capture.intent));
            Ok(())
        }
        Command::Copy { id } => {
            let tin = open()?;
            let capture =
                tin.store.load(&tin.store.resolve(&id).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            context::copy(&capture.clipboard_text())
        }
        Command::Rm { ids } => {
            let tin = open()?;
            for id in ids {
                let id = tin.store.resolve(&id).map_err(|e| e.to_string())?;
                tin.store.remove(&id).map_err(|e| e.to_string())?;
                println!("Removed {id}");
            }
            Ok(())
        }
        Command::Routes { json } => routes(json),
        Command::Init => init(),
        Command::Paths => {
            let paths = Paths::from_env();
            println!("config   {}", paths.config.display());
            println!("captures {}", paths.captures().display());
            Ok(())
        }
    }
}

fn capture(args: CaptureArgs) -> Outcome {
    let tin = open()?;
    if args.voice && !std::io::stdin().is_terminal() {
        return Err(
            "--voice waits for Enter in a terminal; run it in one, or use the Tin app's microphone button.".into()
        );
    }
    // Read the screen first, so the selection and page are the ones from when you asked.
    let text_grab = match (&args.text, args.stdin, args.clipboard, args.no_text) {
        (Some(_), ..) | (_, true, ..) | (.., true) => TextGrab::None,
        (_, _, true, _) => TextGrab::Clipboard,
        _ => TextGrab::Selection,
    };
    let page_wanted = !args.no_page && args.url.is_none();
    let gathered = context::gather(Options { text: text_grab, page: page_wanted, screenshot: args.screenshot });
    // A missing window is routine on some desktops; say so only in `tin context`.
    warn_all(gathered.notes.iter().filter(|n| n.part != Part::Window).map(|n| n.message.clone()));

    let text = match (args.text, args.stdin) {
        (Some(text), _) => Some(CapturedText { text, from: TextSource::Typed }),
        (None, true) => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| format!("couldn't read stdin: {e}"))?;
            Some(CapturedText { text, from: TextSource::Stdin })
        }
        (None, false) => gathered.text,
    };
    let page = match args.url {
        Some(url) => Some(Page::new(url, None, PageSource::Manual)),
        None => gathered.page,
    };

    let voice = if args.voice { record_voice(&tin) } else { None };
    let draft = Draft {
        text,
        page,
        window: gathered.window,
        voice,
        note: args.note,
        screenshot: gathered.screenshot,
        intent: Intent { said: args.purpose, ..Intent::default() },
    };
    let saved = tin.save(draft).map_err(|e| e.to_string())?;
    warn_all(saved.hook_errors.iter().map(ToString::to_string));
    if args.json {
        print_json(&saved.capture);
    } else {
        print_capture(&saved.capture, &saved.file.display().to_string());
    }
    Ok(())
}

/// Records with Hex until Enter. A failure is a warning: the rest of the capture still saves.
fn record_voice(tin: &Tin) -> Option<tin_core::capture::VoiceNote> {
    let hex = tin.hex();
    if let Err(e) = hex.start() {
        warn(&e.to_string());
        return None;
    }
    // Ctrl-C mustn't leave Hex recording, or its next stop would paste into whatever is focused.
    let on_interrupt = hex.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = on_interrupt.cancel();
        eprintln!("\ntin: cancelled; nothing was saved.");
        std::process::exit(130);
    });
    eprint!("Recording a voice note with Hex… press Enter to stop. ");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    match hex.stop() {
        Ok(note) => {
            eprintln!("Heard: {}", note.transcript);
            Some(note)
        }
        Err(e) => {
            warn(&e.to_string());
            None
        }
    }
}

fn show_context(json: bool) -> Outcome {
    let gathered = context::gather(Options::default());
    if json {
        let value = serde_json::json!({
            "text": gathered.text,
            "window": gathered.window,
            "page": gathered.page,
            "notes": gathered.notes,
        });
        println!("{}", serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?);
        return Ok(());
    }
    let none = || "—".to_owned();
    println!("text    {}", gathered.text.as_ref().map_or_else(none, |t| one_line(&t.text, 100)));
    let window = gathered
        .window
        .as_ref()
        .map(|w| [w.app.as_deref(), w.title.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" — "));
    println!("window  {}", window.unwrap_or_else(none));
    println!("page    {}", gathered.page.as_ref().map_or_else(none, describe_page));
    for note in &gathered.notes {
        println!("note    {}", note.message);
    }
    Ok(())
}

fn list(args: ListArgs) -> Outcome {
    let tin = open()?;
    let listing = tin.store.list().map_err(|e| e.to_string())?;
    warn_all(listing.unreadable.iter().map(ToString::to_string));
    let route = args.route.as_deref().map(tag_of);
    let captures: Vec<&Capture> = listing
        .captures
        .iter()
        .filter(|c| match (args.all, args.done) {
            (true, _) => true,
            (false, done) => c.is_done() == done,
        })
        .filter(|c| {
            route
                .as_ref()
                .is_none_or(|r| c.intent.tags.contains(r) || c.intent.target.name().is_some_and(|n| tag_of(n) == *r))
        })
        .take(args.limit.unwrap_or(usize::MAX))
        .collect();
    if args.json {
        print_json(&captures);
        return Ok(());
    }
    if captures.is_empty() {
        eprintln!("Nothing here. Capture something with `tin capture`.");
    }
    for c in captures {
        let check = if c.is_done() { "✓" } else { "○" };
        let when = c.created_at.get(5..16).unwrap_or("").replace('T', " ");
        let routed = c.intent.target.name().map(|n| format!("[{n}] ")).unwrap_or_default();
        let tags: String = c
            .intent
            .tags
            .iter()
            .filter(|t| c.intent.target.name().is_none_or(|n| tag_of(n) != **t))
            .map(|t| format!("#{t} "))
            .collect();
        let page = c.page.as_ref().map(|p| format!("  ({})", p.host())).unwrap_or_default();
        let voice = if c.voice.is_some() { " 🎙" } else { "" };
        println!("{check} {when}  {}  {routed}{tags}{}{voice}{page}", short(c), one_line(&c.headline(), 72));
    }
    Ok(())
}

fn set_done(ids: &[String], done: bool) -> Outcome {
    if ids.is_empty() {
        return Err("name at least one capture (an id, a unique part of one, or `last`)".into());
    }
    let tin = open()?;
    for id in ids {
        let id = tin.store.resolve(id).map_err(|e| e.to_string())?;
        let capture = tin.set_done(&id, done).map_err(|e| e.to_string())?;
        println!("{} {}  {}", if done { "✓" } else { "○" }, short(&capture), one_line(&capture.headline(), 72));
    }
    Ok(())
}

fn routes(json: bool) -> Outcome {
    let tin = open()?;
    if json {
        let routes: Vec<_> = tin
            .routes
            .iter()
            .map(|r| {
                let (kind, path, discovered) = match &r.kind {
                    RouteKind::Project { path, discovered } => ("project", Some(path.display().to_string()), *discovered),
                    RouteKind::Topic => ("topic", None, false),
                };
                serde_json::json!({"name": r.name, "tag": tag_of(&r.name), "kind": kind, "path": path, "discovered": discovered, "hook": r.hook})
            })
            .collect();
        print_json(&routes);
        return Ok(());
    }
    for r in tin.routes.iter() {
        match &r.kind {
            RouteKind::Project { path, discovered: true } => println!("{:<24} project  {}", r.name, path.display()),
            RouteKind::Project { path, discovered: false } => {
                println!("{:<24} project  {}  (configured)", r.name, path.display())
            }
            RouteKind::Topic => println!("{:<24} topic", r.name),
        }
    }
    eprintln!("Configure routes in {}", tin.paths.config.display());
    Ok(())
}

fn init() -> Outcome {
    let path = Paths::from_env().config;
    if path.exists() {
        println!("{} already exists; leaving it alone.", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, tin_core::config::STARTER).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

fn print_capture(c: &Capture, file: &str) {
    let status = match &c.status {
        Status::Open => "open".to_owned(),
        Status::Done { at } => format!("done {at}"),
    };
    println!("{}  {status}  {}", c.id, c.created_at);
    println!("  for     {}", describe_intent(&c.intent));
    if let Some(t) = &c.text {
        println!("  text    {}", one_line(&t.text, 200));
    }
    if let Some(p) = &c.page {
        println!("  page    {}", describe_page(p));
    }
    if let Some(v) = &c.voice {
        println!("  voice   {}", v.transcript);
    }
    if let Some(n) = &c.note {
        println!("  note    {n}");
    }
    if let Some(s) = &c.screenshot {
        println!("  picture {} ({}×{})", s.path.display(), s.width, s.height);
    }
    if let Some(w) = &c.window {
        let w = [w.app.as_deref(), w.title.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" — ");
        println!("  window  {}", one_line(&w, 100));
    }
    println!("  file    {file}");
}

fn describe_intent(intent: &Intent) -> String {
    let target = match &intent.target {
        Target::Undecided => "undecided".to_owned(),
        Target::Project { name, path: Some(path), .. } => format!("project {name} ({})", path.display()),
        Target::Project { name, path: None, .. } => format!("project {name}"),
        Target::NewProject { name: Some(name) } => format!("new project “{name}”"),
        Target::NewProject { name: None } => "a new project".to_owned(),
    };
    let tags: Vec<String> = intent.tags.iter().map(|t| format!("#{t}")).collect();
    let said = intent.said.as_deref().map(|s| format!("  “{s}”")).unwrap_or_default();
    format!("{target}{}{said}", if tags.is_empty() { String::new() } else { format!("  {}", tags.join(" ")) })
}

fn describe_page(p: &Page) -> String {
    let via = match &p.via {
        PageSource::BrowserTab { browser, focused: true } => format!("{browser}'s tab"),
        PageSource::BrowserTab { browser, focused: false } => format!("{browser}'s last tab; window unknown"),
        PageSource::CapturedText => "from the text".to_owned(),
        PageSource::Manual => "given".to_owned(),
    };
    match &p.kind {
        PageKind::Tweet { handle, text, .. } => {
            let text = text.as_deref().map(|t| format!(": {}", one_line(t, 80))).unwrap_or_default();
            format!("tweet by @{handle}{text}  {}  ({via})", p.url)
        }
        PageKind::Link => {
            let title = p.title.as_deref().map(|t| format!("{}  ", one_line(t, 60))).unwrap_or_default();
            format!("{title}{}  ({via})", p.url)
        }
    }
}

fn short(c: &Capture) -> &str {
    c.id.as_str().get(20..).unwrap_or(c.id.as_str())
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max { flat } else { format!("{}…", flat.chars().take(max - 1).collect::<String>()) }
}

fn print_json<T: serde::Serialize + ?Sized>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(e) => warn(&format!("couldn't print JSON: {e}")),
    }
}

fn warn(message: &str) {
    eprintln!("tin: {message}");
}

fn warn_all(messages: impl Iterator<Item = String>) {
    for m in messages {
        warn(&m);
    }
}
