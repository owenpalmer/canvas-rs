# canvas-rs

Your Canvas LMS account, available locally, signed in with the Firefox session you already have. No
API token, no school approval, no passwords stored. Written in Rust; the desktop app draws itself
on the GPU (wgpu), with no web view.

It has two parts that share one local cache:

- **An MCP server** (`canvas-mcp`) that gives an LLM (Claude, or any MCP client) read-only access
  to your courses, assignments, grades, announcements, modules, pages, files, discussions, and inbox.
- **A desktop app** (`canvas-app`), a faster alternative interface to Canvas that keeps working from
  its cache when you're offline or your session has expired. It also has a PDF viewer with
  checkpoints (Claude writes retrieval-practice questions about what you just read), Anki review,
  Panopto recordings, and export to NotebookLM.

Not affiliated with Instructure, Panopto, Anthropic, or Google. Works on Linux and Windows, with
Firefox only: Chrome and Edge encrypt cookies in a way other programs can't read on Windows.

## Install

From the repository's **Releases** page:

- **Linux:** `canvas-rs-linux-x64.tar.gz`. Unpack it and run `./install.sh`: it puts the app in
  `~/.local/share/canvas-rs`, adds **Canvas** to your app launcher (with a "Sample data" action),
  and links `canvas-app`, `canvas-mcp` and `canvas-check` into `~/.local/bin`. Run a newer
  release's `install.sh` to update.
- **Windows:** `canvas-rs-windows-x64.zip`. Unzip it anywhere and run `canvas-app.exe`. Windows may
  say "Windows protected your PC" the first time, because the app isn't code-signed; click
  *More info → Run anyway*.

## Build

Needs a Rust toolchain (edition 2024) and PDFium for the PDF viewer:

```bash
scripts/fetch-pdfium.sh            # PDFium into vendor/pdfium/linux-x64 (or: scripts/fetch-pdfium.sh win-x64)
cargo build --release
```

`scripts/package-linux.sh` then makes the Linux release tarball (in `dist/`). Tagging `v*`
builds and publishes both platforms' releases (`.github/workflows/release.yml`).

This gives `target/release/canvas-app`, `target/release/canvas-mcp` and
`target/release/canvas-check`. On Linux the app needs the usual windowing libraries (`libxkbcommon`,
Wayland or X11) and a GPU driver with Vulkan or OpenGL. A packaged app looks for PDFium next to its
executable, or in `lib/` beside it.

## How signing in works (and what it reads)

Instead of an API token, it reuses your existing browser login. It reads cookies from your default
Firefox profile, and only for sites you've allowed:

- **Permission first:** nothing is read for a site until you allow that site: your Canvas host,
  your Panopto host, and Google (for NotebookLM) are each asked for separately, the first time
  they're needed. Talking to Anki and sending passages to Claude are permissions of their own too.
  Settings → Permissions in the app shows what you've allowed and takes it back; grants are stored
  as `permissions` in `config.toml`.
- **Which cookies:** only those for your Canvas host and, if you use them, your Panopto host and
  Google's NotebookLM domains. Cookies for every other site are skipped.
- **How:** it copies Firefox's `cookies.sqlite` (and session store) to a private temporary folder
  and reads the copy, so Firefox's own files are never modified or locked.
- **Where they go:** only to the site they belong to, over HTTPS. They're never logged, printed,
  written to disk, or sent anywhere else. The check commands print cookie *names*, never values.

Everything it does against Canvas is read-only (the MCP tools only `GET`).

If Firefox isn't set to restore your previous session, it deletes session cookies when it closes,
and many schools' Canvas logins are session cookies. If you have to log in again every time you open
Firefox, turn on *Settings → General → Startup → Open previous windows and tabs*. `canvas-check`
tells you when this setting is off.

## Set up

Open the desktop app. On first launch it asks for your school (type its Canvas address, or just its
short name, like `uw`), asks permission to use your Canvas login from Firefox, and opens your
school's login page in Firefox if you aren't logged in yet. As soon as you are, it shows who you're
signed in as and loads your courses.

To set it up by hand instead, create `~/.canvas-mcp/config.toml` (on Windows,
`C:\Users\<you>\.canvas-mcp\config.toml`):

```toml
canvas_url = "https://canvas.your-school.edu"      # required
panopto_host = "your-school.hosted.panopto.com"    # optional, for lecture recordings
```

Every setting can also be set by environment variable; see the top of
[`crates/canvas-mcp/src/config.rs`](crates/canvas-mcp/src/config.rs) for the full list.

Then check that it works (the first run asks permission to read your Canvas cookies):

```bash
canvas-check              # prints the cookie names found and who you're logged in as
canvas-check --diagnose   # if it can't find your session: where it looked, per profile
```

## The MCP server

Add it to your MCP client. For Claude Code:

```bash
claude mcp add canvas -- /path/to/canvas-mcp
```

For Claude Desktop, in `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "canvas": { "command": "/path/to/canvas-mcp" }
  }
}
```

Tools: `whoami`, `list_courses`, `upcoming`, `list_assignments`, `get_assignment`, `grades`,
`announcements`, `list_modules`, `get_page`, `list_pages`, `syllabus`, `list_files`,
`download_file`, `discussions`, `inbox`, `get_conversation`, and `api_get` (any read-only Canvas
API path).

Reads go through the same cache as the desktop app (`~/.canvas-mcp/cache.db`). Data that's older
than its refresh interval is refreshed first; if Canvas can't be reached (offline, slow, or the
session expired), the tool returns the cached copy with a note saying how old it is.

## The desktop app

```bash
canvas-app            # your Canvas
canvas-app --demo     # sample data, no Canvas account needed
```

It syncs your active courses in the background every few minutes and serves everything from the
cache, so pages open instantly and stay readable offline. Keyboard: `Ctrl K` or `/` to search,
`g d` dashboard, `g i` inbox, `g a` Anki, `r` refresh, `t` theme, `?` for all shortcuts.
Appearance settings (including the procedural vines) are under Settings.

**Search** (`Ctrl K` or `/`) finds titles, and also text inside everything cached: pages,
assignment descriptions, announcements, discussions, the syllabus, messages, PDFs and lecture
transcripts (from Panopto, when it's set up). Choosing a hit opens it in the viewer, scrolled to
the passage, which is briefly highlighted. The index lives in `cache.db` and is brought up to date
after each sync.

**PDFs and checkpoints.** PDFs open in the viewer pane. Press `M` (or *Checkpoints*) in a PDF to add
checkpoints: `J`/`K` step through the text sentence by sentence, `C` twice splits after the
highlighted one, and `A` places one with a click. Claude then writes questions about the passage
above it (back to the previous checkpoint, at most two pages), using your Anthropic API key (Settings
→ Checkpoints; stored in your system keychain). Good questions go to Anki with a picture of the
passage as the hint. Math (`\( … \)`, `\[ … \]`, `\ce{…}`) and molecules (`\smiles{…}`) are drawn
in the questions and in course pages.

## Optional integrations

**Panopto recordings.** With `panopto_host` set and a Panopto login in Firefox, the app finds each
course's recordings folder (from links in the course, or by searching for the course code), so
their captions can be added to NotebookLM notebooks as transcripts.

**NotebookLM export.** Sends pages, assignments, files, and recording transcripts from a course to a
NotebookLM notebook. Be aware:

- It needs your **Google** session, which is much broader than a Canvas session: it's your whole
  Google account. Only the Google cookies NotebookLM needs are read, and they're only held in
  memory, but that's still a sensitive credential. Don't allow Google if you don't want this.
- It talks to NotebookLM's private API, which can change without notice, and automated use may
  conflict with Google's terms.

**Anki review.** The app's Anki page reviews your Anki decks through the
[AnkiConnect](https://ankiweb.net/shared/info/2055492159) add-on. If it isn't installed, the page
offers to set Anki up: it downloads AnkiConnect from AnkiWeb into Anki's add-ons folder, gives it a
random API key, and adds a small companion add-on ([`assets/anki_companion`](assets/anki_companion))
so the app can open Anki in the tray, without its window. Anki restarts if it was open. The
companion does nothing when you open Anki yourself. You can also install AnkiConnect yourself:
Tools → Add-ons → Get Add-ons…, code `2055492159`.

Anki does all the scheduling, so reviews here count exactly as they do in Anki (card audio doesn't
play). "Import course decks" links a course to one of your existing decks (matching ones are
suggested) or creates a new one under a `Canvas` parent deck (`anki_parent_deck` in
`config.toml`). Keyboard: `Space` shows the answer (then means Good), `1`–`4` answer
Again/Hard/Good/Easy.

## Your data

Everything lives in `~/.canvas-mcp/` (or `CANVAS_MCP_DIR`), readable only by you:

| File | What |
|---|---|
| `config.toml` | your settings and permissions |
| `cache.db` | cached Canvas API responses, notebooks, Anki deck links (SQLite) |
| `blobs/` | downloaded files and images |
| `settings.json` | the app's preferences |
| `checkpoints/` | each PDF's checkpoints and questions |

Delete the folder to remove everything. The demo (`--demo`) keeps its own in `~/.canvas-mcp/demo/`.

## Development

```bash
cargo test --workspace
cargo run -p canvas-app -- --demo
cargo run -p canvas-mcp -- smoke          # call every MCP tool (CANVAS_DEMO=1 for sample data)
```

For screenshots and scripted checks the app takes
`--screenshot out.png --wait 3 --size 1280x860 --actions "go:#/anki;key:j"`. To time the launch, run
it with `CANVAS_TIMING=1`: each launch appends the milliseconds from process start to each step to
`~/.canvas-mcp/launch-timing.log`.

The crates:

- `crates/canvas-mcp`: the library and the `canvas-mcp` / `canvas-check` binaries: config and
  permissions, Firefox cookies, the Canvas client, the stale-while-revalidate cache, the MCP server,
  Panopto, NotebookLM, AnkiConnect, and writing checkpoint questions with Claude.
- `crates/canvas-app`: the desktop app (eframe/egui on wgpu): the panes and views, the HTML
  renderer, the PDF viewer (PDFium), math, molecules, the paper and vine effects.

Fonts: Inter (SIL Open Font License) and KaTeX's fonts (MIT); their licenses are in `assets/fonts`.
