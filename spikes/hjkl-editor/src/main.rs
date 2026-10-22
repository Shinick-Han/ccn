//! Spike: embed the hjkl Vim engine (crates only, no `hjkl` app) in ratatui.
//!
//! Not a product feature. See ../README.md for the evaluation.

#[cfg_attr(not(feature = "syntax"), allow(unused_imports))]
use std::path::{Path, PathBuf};
#[cfg(feature = "syntax")]
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use hjkl_buffer_tui::{BufferView, Gutter, GutterNumbers};
use hjkl_engine::types::DefaultHost;
use hjkl_engine::{Editor, Host, Options};
#[cfg_attr(not(feature = "syntax"), allow(unused_imports))]
use hjkl_engine_tui::EditorRatatuiExt;
use hjkl_vim::{VimEditorExt, dispatch_input, vim_editor};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[cfg(feature = "syntax")]
use hjkl_bonsai::Highlighter;
#[cfg(feature = "syntax")]
use hjkl_bonsai::runtime::{Grammar, GrammarLoader, GrammarRegistry};
#[cfg(feature = "syntax")]
use hjkl_bonsai::theme::{DotFallbackTheme, Theme};

type Ed = Editor<hjkl_buffer::View, DefaultHost>;

/// Everything the spike owns; the editor itself is entirely `hjkl`.
struct App {
    ed: Ed,
    path: PathBuf,
    /// Engine stores `\n` only; no `fileformat` option exists (unlike vim), so
    /// the host remembers the on-disk EOL and converts back on save.
    crlf: bool,
    /// Set when the buffer has been written since the last edit.
    status: String,
    /// `Some` while the host is collecting a `:` command line.
    cmdline: Option<String>,
    syntax: Option<SyntaxState>,
    syntax_note: String,
    quit: bool,
}

#[cfg(feature = "syntax")]
struct SyntaxState {
    hl: Highlighter,
    theme: DotFallbackTheme,
    last_text: String,
}

#[cfg(not(feature = "syntax"))]
struct SyntaxState;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = args.iter().any(|a| a == "--headless");
    let render_once = args.iter().any(|a| a == "--render-once");
    let no_syntax = args.iter().any(|a| a == "--no-syntax");
    let keys = args
        .windows(2)
        .find(|w| w[0] == "--keys")
        .map(|w| w[1].clone());
    let path: PathBuf = args
        .iter()
        .find(|a| !a.starts_with("--") && !keys.as_deref().is_some_and(|k| k == *a))
        .map(PathBuf::from)
        .ok_or("usage: hjkl-spike <file> [--no-syntax] | --headless <file> --keys \"ihello<Esc>:w<CR>\"")?;

    let mut app = App::open(path, !no_syntax)?;

    if headless {
        for input in parse_keys(&keys.unwrap_or_default()) {
            app.handle(input);
        }
        let text = app.ed.content();
        let on_disk = std::fs::read_to_string(&app.path)?;
        println!(
            "headless mode={:?} cursor={:?} lines={} bytes={} on_disk_bytes={} status={:?} {}",
            app.ed.vim_mode(),
            app.ed.cursor(),
            text.lines().count(),
            text.len(),
            on_disk.len(),
            app.status,
            app.syntax_note
        );
        println!("content={:?}", text);
        return Ok(());
    }

    if render_once {
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(100, 20))?;
        term.draw(|f| draw(f, &mut app))?;
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            println!("|{}|", row.trim_end());
        }
        return Ok(());
    }

    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let res = run_ui(&mut term, &mut app);
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    res?;
    Ok(())
}

impl App {
    fn open(path: PathBuf, syntax: bool) -> Result<Self, Box<dyn std::error::Error>> {
        // Engine contract (undocumented, found empirically): `content()` always
        // ends with "\n", while `set_content` keeps a trailing empty row. Strip
        // exactly one trailing newline on load or every save grows the file.
        // CRLF: hjkl has no `fileformat`; normalise here and restore on save.
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let crlf = raw.contains("\r\n");
        let mut text = raw.replace("\r\n", "\n");
        if text.ends_with('\n') {
            text.pop();
        }
        let mut ed = vim_editor(
            hjkl_buffer::View::new(),
            DefaultHost::new(),
            Options::default(),
        );
        ed.set_content(&text);
        let mut app = Self {
            ed,
            path,
            crlf,
            status: String::new(),
            cmdline: None,
            syntax: None,
            syntax_note: "syntax: off".to_string(),
            quit: false,
        };
        if syntax {
            #[cfg(feature = "syntax")]
            match SyntaxState::attach(&app.path) {
                Ok((s, lang)) => {
                    app.syntax = Some(s);
                    app.syntax_note = format!("syntax: tree-sitter/{lang}");
                }
                Err(e) => app.syntax_note = format!("syntax: off ({e})"),
            }
            #[cfg(not(feature = "syntax"))]
            {
                app.syntax_note = "syntax: not compiled in (build feature `syntax` off)".into();
            }
        }
        Ok(app)
    }

    fn handle(&mut self, input: hjkl_engine::Input) {
        // Host-owned command line: `:` is a host concern in vim, the engine
        // never sees it (this is the app's `:`, not the engine's).
        if let Some(buf) = self.cmdline.as_mut() {
            match input.key {
                hjkl_engine::Key::Esc => self.cmdline = None,
                hjkl_engine::Key::Enter => {
                    let cmd = self.cmdline.take().unwrap_or_default();
                    self.run_ex(&cmd);
                }
                hjkl_engine::Key::Backspace => {
                    buf.pop();
                    if buf.is_empty() {
                        self.cmdline = None;
                    }
                }
                hjkl_engine::Key::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }

        if self.ed.vim_mode() == hjkl_engine::VimMode::Normal {
            if let hjkl_engine::Key::Char(':') = input.key {
                self.cmdline = Some(String::new());
                return;
            }
        }

        dispatch_input(&mut self.ed, input);
        self.refresh_syntax();
    }

    fn run_ex(&mut self, cmd: &str) {
        let reg = hjkl_ex::default_registry::<DefaultHost>();
        let effect = hjkl_ex::try_dispatch(&reg, &mut self.ed, cmd)
            .unwrap_or(hjkl_ex::ExEffect::Unknown(cmd.to_string()));
        match effect {
            hjkl_ex::ExEffect::Save => match self.save() {
                Ok(n) => self.status = format!("written {n} bytes"),
                Err(e) => self.status = format!("write failed: {e}"),
            },
            hjkl_ex::ExEffect::SaveAs(p) => {
                self.path = PathBuf::from(p);
                match self.save() {
                    Ok(n) => self.status = format!("written {n} bytes"),
                    Err(e) => self.status = format!("write failed: {e}"),
                }
            }
            hjkl_ex::ExEffect::Quit { save, .. } => {
                if save && let Err(e) = self.save() {
                    self.status = format!("write failed: {e}");
                    return;
                }
                self.quit = true;
            }
            hjkl_ex::ExEffect::Substituted { count, .. } => {
                self.status = format!("{count} substitution(s)");
                self.refresh_syntax();
            }
            hjkl_ex::ExEffect::Ok | hjkl_ex::ExEffect::None => {}
            hjkl_ex::ExEffect::Info(m) | hjkl_ex::ExEffect::Error(m) => self.status = m,
            other => self.status = format!("{other:?}"),
        }
    }

    fn save(&self) -> std::io::Result<usize> {
        let text = self.ed.content();
        let out = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text
        };
        std::fs::write(&self.path, &out)?;
        Ok(out.len())
    }

    #[cfg(feature = "syntax")]
    fn refresh_syntax(&mut self) {
        let Some(s) = self.syntax.as_mut() else {
            return;
        };
        let text = self.ed.content();
        if text == s.last_text {
            return;
        }
        let spans = s.spans_for(&text);
        s.last_text = text;
        self.ed.install_ratatui_syntax_spans(&spans);
    }

    #[cfg(not(feature = "syntax"))]
    fn refresh_syntax(&mut self) {}
}

#[cfg(feature = "syntax")]
impl SyntaxState {
    /// Attach highlighting *only from already-installed grammar artifacts*.
    /// Never downloads or compiles (that is `GrammarLoader::load`).
    fn attach(path: &Path) -> Result<(Self, String), String> {
        let reg = GrammarRegistry::embedded().map_err(|e| e.to_string())?;
        let name = reg.name_for_path(path).ok_or("no language for extension")?;
        let spec = reg.by_name(name).ok_or("language not in manifest")?;
        let _ = spec;
        let loader =
            GrammarLoader::user_default(reg.meta()).map_err(|e| format!("loader: {e}"))?;
        let so = loader
            .lookup_only(name)
            .ok_or_else(|| format!("grammar '{name}' not installed in {}", loader.user_dir().display()))?;
        let grammar = Grammar::load_from_path(name, &so).map_err(|e| format!("dlopen: {e}"))?;
        let hl = Highlighter::new(Arc::new(grammar)).map_err(|e| format!("highlighter: {e}"))?;
        Ok((
            Self {
                hl,
                theme: DotFallbackTheme::dark(),
                last_text: String::new(),
            },
            name.to_string(),
        ))
    }

    /// `Vec<row>[ (start_char, end_char, style) ]` as the engine wants it.
    fn spans_for(&mut self, text: &str) -> Vec<Vec<(usize, usize, Style)>> {
        let src = text.as_bytes();
        let raw = self.hl.highlight(src);
        let mut line_starts = vec![0usize];
        for (i, b) in src.iter().enumerate() {
            if *b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let mut rows: Vec<Vec<(usize, usize, Style)>> = vec![Vec::new(); line_starts.len()];
        for span in raw {
            let Some(style) = self.theme.style(&span.capture) else {
                continue;
            };
            let style = style_spec_to_ratatui(style);
            let row = line_starts.partition_point(|&s| s <= span.byte_range.start) - 1;
            if let Some(row_spans) = rows.get_mut(row) {
                let ls = line_starts[row];
                let le = line_starts.get(row + 1).copied().unwrap_or(src.len());
                let s = span.byte_range.start.max(ls).min(le);
                let e = span.byte_range.end.max(s).min(le);
                let cs = text[ls..s].chars().count();
                let ce = cs + text[s..e].chars().count();
                if ce > cs {
                    row_spans.push((cs, ce, style));
                }
            }
        }
        rows
    }
}

#[cfg(feature = "syntax")]
fn style_spec_to_ratatui(s: &hjkl_bonsai::theme::StyleSpec) -> Style {
    let mut st = Style::default();
    if let Some(c) = s.fg {
        st = st.fg(Color::Rgb(c.r, c.g, c.b));
    }
    if let Some(c) = s.bg {
        st = st.bg(Color::Rgb(c.r, c.g, c.b));
    }
    if s.modifiers.bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.modifiers.italic {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.modifiers.underline {
        st = st.add_modifier(Modifier::UNDERLINED);
    }
    if s.modifiers.reverse {
        st = st.add_modifier(Modifier::REVERSED);
    }
    st
}

fn run_ui(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last = Instant::now();
    loop {
        term.draw(|f| draw(f, app))?;
        // Cheap poll so the UI stays responsive without a full redraw loop.
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(k) => {
                    let input = hjkl_engine_tui::crossterm_to_input(k);
                    if k.kind != KeyEventKind::Release
                        && !matches!(input.key, hjkl_engine::Key::Null)
                    {
                        app.handle(input);
                    }
                }
                Event::Resize(..) => {}
                _ => {}
            }
        }
        if app.quit {
            return Ok(());
        }
        if last.elapsed() > Duration::from_secs(2) {
            last = Instant::now();
        }
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(72), Constraint::Percentage(28)])
        .split(chunks[0]);
    let editor_area = body[0];
    let sidebar_area = body[1];

    // Publish geometry to the engine's Host: the engine owns scroll state but
    // the host owns the window size. This is the whole "integration".
    let gutter_w = 4u16;
    let vp = app.ed.host_mut().viewport_mut();
    vp.width = editor_area.width;
    vp.height = editor_area.height;
    vp.text_width = editor_area.width.saturating_sub(gutter_w);
    vp.tab_width = 4;

    let spans = app.ed.buffer_spans();
    let buffer = app.ed.buffer();
    let viewport = app.ed.host().viewport();
    let selection = app.ed.buffer_selection();
    let style_table = app.ed.style_table().to_vec();
    let resolver = move |id: u32| {
        style_table
            .get(id as usize)
            .copied()
            .map(hjkl_engine_tui::style_to_ratatui)
            .unwrap_or_default()
    };
    let lines = buffer.rope().len_lines();
    let gw = lines.to_string().len() as u16 + 1;
    let widget = BufferView {
        buffer,
        viewport,
        selection,
        resolver: &resolver,
        cursor_line_bg: Style::default().bg(Color::Rgb(30, 32, 40)),
        cursor_line_row: None,
        fold_line_bg: Style::default(),
        folds_override: None,
        cursor_column_bg: Style::default(),
        cursor_column: None,
        selection_bg: Style::default().bg(Color::Rgb(48, 52, 70)),
        cursor_style: Style::default().add_modifier(Modifier::REVERSED),
        gutter: Some(Gutter {
            width: gw,
            style: Style::default().fg(Color::Rgb(90, 96, 110)),
            line_offset: 0,
            numbers: GutterNumbers::Absolute,
            sign_column_width: 0,
            fold_column_width: 0,
        }),
        search_bg: Style::default().bg(Color::Rgb(90, 80, 30)),
        signs: &[],
        conceals: &[],
        spans,
        search_pattern: None,
        search_ranges: None,
        non_text_style: Style::default().fg(Color::Rgb(60, 64, 76)),
        show_eob: true,
        diag_overlays: &[],
        colorcolumn_cols: &[],
        colorcolumn_style: Style::default(),
        listchars: None,
        indent_guides_enabled: false,
        indent_guide_char: ' ',
        indent_guide_shiftwidth: 4,
        indent_guide_fg: Color::Reset,
        indent_guide_active_fg: Color::Reset,
        indent_guide_active_col: None,
        eol_hints: &[],
        blame_plan: None,
        diff_filler: None,
        background: Style::default(),
    };
    f.render_widget(widget, editor_area);

    draw_sidebar(f, app, sidebar_area);

    let mode = format!("{:?}", app.ed.vim_mode()).to_uppercase();
    let (line, col) = app.ed.cursor();
    let name = app
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let syn = if app.syntax.is_some() { "ts" } else { "plain" };
    let status = Line::from(vec![
        Span::styled(
            format!(" {mode} "),
            Style::default()
                .bg(Color::Rgb(80, 120, 200))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {name}  {line}:{col}  [{syn}]  ")),
        Span::styled(&app.status, Style::default().fg(Color::Rgb(200, 180, 120))),
    ]);
    f.render_widget(Paragraph::new(status), chunks[1]);

    let cmd = match &app.cmdline {
        Some(s) => format!(":{s}"),
        None => String::new(),
    };
    f.render_widget(
        Paragraph::new(Line::from(cmd)).style(Style::default().bg(Color::Rgb(24, 26, 32))),
        chunks[2],
    );
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let (line, col) = app.ed.cursor();
    let body = vec![
        Line::from(Span::styled(
            "hjkl spike",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("embedding check:"),
        Line::from("  hjkl-engine + hjkl-vim + hjkl-ex"),
        Line::from("  hjkl-buffer-tui (paint)"),
        Line::from("  hjkl-bonsai (optional)"),
        Line::from(""),
        Line::from(format!("file   {}", app.path.display())),
        Line::from(format!("cursor {}:{}", line, col)),
        Line::from(format!("lines  {}", app.ed.buffer().rope().len_lines())),
        Line::from(""),
        Line::from("i a o  insert   Esc normal"),
        Line::from("v V    visual   :w :q"),
        Line::from("dd yy p u      hjkl FSM"),
        Line::from(""),
        Line::from(Span::styled(
            "(placeholder sidebar)",
            Style::default().fg(Color::Rgb(110, 115, 130)),
        )),
    ];
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::Rgb(50, 54, 64))),
        ),
        area,
    );
}

// --- headless key script ---------------------------------------------------

fn parse_keys(spec: &str) -> Vec<hjkl_engine::Input> {
    let mut out = Vec::new();
    let mut chars = spec.chars().peekable();
    let key = |c: char| hjkl_engine::Input {
        key: hjkl_engine::Key::Char(c),
        ctrl: false,
        alt: false,
        shift: false,
    };
    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(key(c));
            continue;
        }
        let mut name = String::new();
        for n in chars.by_ref() {
            if n == '>' {
                break;
            }
            name.push(n);
        }
        let input = match name.as_str() {
            "Esc" => hjkl_engine::Input {
                key: hjkl_engine::Key::Esc,
                ctrl: false,
                alt: false,
                shift: false,
            },
            "CR" | "Enter" => hjkl_engine::Input {
                key: hjkl_engine::Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            "BS" => hjkl_engine::Input {
                key: hjkl_engine::Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            },
            "Tab" => hjkl_engine::Input {
                key: hjkl_engine::Key::Tab,
                ctrl: false,
                alt: false,
                shift: false,
            },
            other => {
                if let Some(c) = other.strip_prefix("C-").and_then(|s| s.chars().next()) {
                    hjkl_engine::Input {
                        key: hjkl_engine::Key::Char(c),
                        ctrl: true,
                        alt: false,
                        shift: false,
                    }
                } else {
                    continue;
                }
            }
        };
        out.push(input);
    }
    out
}
