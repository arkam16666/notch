//! Reads "now playing" info and sends playback commands. Runs on a
//! background thread.
//!
//! Sources:
//! - Spotify / Apple Music via their AppleScript dictionaries
//! - YouTube / YouTube Music in Chrome-family browsers and Safari:
//!   * with "Allow JavaScript from Apple Events" enabled we run a small JS
//!     snippet in the tab → full info (play state, position) + direct control
//!   * without it we still read the tab URL → thumbnail + title/channel via
//!     YouTube oEmbed; control falls back to media keys (see main.rs)
//!
//! (The private MediaRemote framework is locked to Apple-signed processes
//! since macOS 15.4, so AppleScript is the reliable public route.)

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Spotify,
    Music,
    /// Chromium-based browser that shares Chrome's AppleScript dictionary.
    Chromium(&'static str),
    Safari,
}

/// Browsers we look for YouTube tabs in (process name / app name).
const CHROMIUMS: [&str; 3] = ["Google Chrome", "Brave Browser", "Microsoft Edge"];

impl App {
    fn process_name(self) -> &'static str {
        match self {
            App::Spotify => "Spotify",
            App::Music => "Music",
            App::Chromium(name) => name,
            App::Safari => "Safari",
        }
    }
    pub fn bundle_id(self) -> &'static str {
        match self {
            App::Spotify => "com.spotify.client",
            App::Music => "com.apple.Music",
            App::Chromium("Brave Browser") => "com.brave.Browser",
            App::Chromium("Microsoft Edge") => "com.microsoft.edgemac",
            App::Chromium(_) => "com.google.Chrome",
            App::Safari => "com.apple.Safari",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub app: App,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub id: String,
    pub position: f64,
    pub duration: f64,
    pub artwork_url: Option<String>,
    /// For browser tabs: (window id, tab id / tab index).
    pub tab: Option<(String, String)>,
    /// false = browser tab without JS access: play state / position unknown,
    /// control must go through media keys.
    pub full: bool,
    /// The player's own volume 0..1 (not the system volume), if known.
    pub volume: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    PlayPause,
    Next,
    Previous,
    /// Jump to this many seconds into the track.
    Seek(f64),
    /// Player volume 0..1.
    SetVolume(f64),
    /// Bring the player (app or browser tab) to the front.
    Focus,
    /// Read the playlist / up-next list (→ Msg::Queue).
    FetchQueue,
    /// Play an item from that list (its `key`, a number).
    PlayItem(u32),

}

pub enum Msg {
    Track(Option<Track>),
    Artwork(Option<Vec<u8>>),
    Queue(Result<Queue, String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct QItem {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Queue {
    /// "เพลย์ลิสต์ …", "คิวเพลง", "ถัดไป"
    pub title: String,
    pub items: Vec<QItem>,
}

fn osascript(script: &str) -> Option<String> {
    let out = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        if std::env::var_os("NOTCH_DEBUG").is_some() {
            eprintln!("osascript error: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
}

fn curl(url: &str) -> Option<Vec<u8>> {
    let out = Command::new("/usr/bin/curl")
        .args(["-sfL", "--max-time", "5", url])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

fn is_running(app: App) -> bool {
    Command::new("/usr/bin/pgrep")
        .args(["-x", app.process_name()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn num_opt(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse().ok()
}

fn num(s: &str) -> f64 {
    s.trim().replace(',', ".").parse().unwrap_or(0.0)
}

/// Escape a string so it can sit inside an AppleScript "..." literal.
fn as_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------------------------------------------------------------- music apps

fn query_music_app(app: App) -> Option<Track> {
    let script = match app {
        App::Spotify => {
            r#"tell application "Spotify"
                if player state is stopped then return ""
                set t to current track
                return (player state as text) & tab & (name of t) & tab & (artist of t) & tab & (id of t) & tab & (player position as text) & tab & (((duration of t) / 1000) as text) & tab & (artwork url of t) & tab & (sound volume as text)
            end tell"#
        }
        App::Music => {
            r#"tell application "Music"
                if player state is stopped then return ""
                try
                    set t to current track
                    return (player state as text) & tab & (name of t) & tab & (artist of t) & tab & (persistent ID of t) & tab & (player position as text) & tab & ((duration of t) as text) & tab & "" & tab & (sound volume as text)
                on error
                    return ""
                end try
            end tell"#
        }
        _ => return None,
    };
    let out = osascript(script)?;
    let f: Vec<&str> = out.split('\t').collect();
    if f.len() < 6 {
        return None;
    }
    Some(Track {
        app,
        playing: f[0] == "playing",
        title: f[1].to_string(),
        artist: f[2].to_string(),
        id: f[3].to_string(),
        position: num(f[4]),
        duration: num(f[5]),
        artwork_url: f.get(6).filter(|s| !s.is_empty()).map(|s| s.to_string()),
        tab: None,
        full: true,
        volume: f.get(7).and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v / 100.0),
    })
}

// ------------------------------------------------------------- YouTube titles

/// Remove (…)/[…] groups that are just noise like "(Official Music Video)".
fn strip_noise_groups(s: &str) -> String {
    const NOISE: [&str; 12] = [
        "official", "mv", "m/v", "lyric", "audio", "video", "visualizer", "4k", "hd",
        "performance", "teaser", "color coded",
    ];
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find(['(', '[']) {
        let close = if rest.as_bytes()[start] == b'(' { ')' } else { ']' };
        let Some(len) = rest[start..].find(close) else { break };
        let group = &rest[start + 1..start + len];
        let lower = group.to_ascii_lowercase();
        out.push_str(&rest[..start]);
        if !NOISE.iter().any(|n| lower.contains(n)) {
            out.push_str(&rest[start..=start + len]);
        }
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// Strip trailing words like "Official Music Video" / "MV".
fn strip_noise_suffix(s: &str) -> String {
    const SUFFIX: [&str; 9] = [
        "official music video", "official video", "official audio", "official mv",
        "music video", "lyric video", "performance video", "m/v", "mv",
    ];
    let mut s = s.trim().to_string();
    loop {
        let lower = s.to_ascii_lowercase();
        let Some(suf) = SUFFIX.iter().find(|x| {
            lower.ends_with(*x)
                && (lower.len() == x.len()
                    || !lower.as_bytes()[lower.len() - x.len() - 1].is_ascii_alphanumeric())
        }) else {
            break;
        };
        s.truncate(s.len() - suf.len());
        s = s.trim_end_matches([' ', '-', '|', '–']).to_string();
    }
    s
}

/// Turn "IVE, David Guetta - Supernova Love Official Music Video" into
/// ("Supernova Love", "IVE, David Guetta").
fn tidy_youtube_title(title: &str, channel: &str) -> (String, String) {
    let t = strip_noise_suffix(&strip_noise_groups(title));
    let channel = channel
        .trim_end_matches(" - Topic")
        .trim_end_matches("VEVO")
        .trim()
        .to_string();
    for sep in [" - ", " – ", " — ", " | "] {
        if let Some((a, b)) = t.split_once(sep) {
            if !a.trim().is_empty() && !b.trim().is_empty() {
                return (b.trim().to_string(), a.trim().to_string());
            }
        }
    }
    // K-pop style: IVE 'REBEL HEART'
    for (open, close) in [('\'', '\''), ('‘', '’'), ('"', '"'), ('“', '”')] {
        if let Some(i) = t.find(open) {
            let after = &t[i + open.len_utf8()..];
            if let Some(j) = after.find(close) {
                let artist = t[..i].trim();
                let song = after[..j].trim();
                if !artist.is_empty() && !song.is_empty() {
                    return (song.to_string(), artist.to_string());
                }
            }
        }
    }
    (t, channel)
}

fn video_id(url: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("v="))
        .map(|v| v.split('#').next().unwrap_or(v).to_string())
}

fn thumbnail(id: &str) -> String {
    format!("https://i.ytimg.com/vi/{id}/maxresdefault.jpg")
}

/// (title, channel) from YouTube oEmbed — no browser permission needed.
fn oembed(id: &str) -> Option<(String, String)> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<(String, String)>>>> =
        LazyLock::new(Default::default);
    if let Some(v) = CACHE.lock().unwrap().get(id) {
        return v.clone();
    }
    let url = format!(
        "https://www.youtube.com/oembed?format=json&url=https://www.youtube.com/watch?v={id}"
    );
    let v = curl(&url)
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .map(|j| {
            (
                j["title"].as_str().unwrap_or("").to_string(),
                j["author_name"].as_str().unwrap_or("").to_string(),
            )
        });
    CACHE.lock().unwrap().insert(id.to_string(), v.clone());
    v
}

// ------------------------------------------------------------------ browsers

/// Runs inside a YouTube tab. Returns tab-separated:
/// state, currentTime, duration, title, artist, artworkUrl, videoId, host
const YT_QUERY_JS: &str = r#"(function(){
var v=document.querySelector('video');if(!v)return '';
var m=navigator.mediaSession&&navigator.mediaSession.metadata;
var c=function(s){return String(s||'').replace(/\s+/g,' ').trim();};
var id=new URLSearchParams(location.search).get('v')||'';
var art='';
if(m&&m.artwork&&m.artwork.length){art=m.artwork[m.artwork.length-1].src;}
var title=(m&&m.title)||document.title.replace(/^\(\d+\)\s*/,'').replace(/ - YouTube( Music)?$/,'');
var artist=(m&&m.artist)||'';
if(!artist){var o=document.querySelector('#owner #channel-name a, ytd-channel-name a');if(o)artist=o.textContent;}
var p=document.getElementById('movie_player');
var vol=(p&&p.getVolume)?((p.isMuted&&p.isMuted())?0:p.getVolume()/100):(v.muted?0:v.volume);
return [v.paused?'paused':'playing',v.currentTime,isFinite(v.duration)?v.duration:0,c(title),c(artist),art,id||location.pathname,location.host,vol].join(String.fromCharCode(9));
})()"#;

const YT_PLAYPAUSE_JS: &str =
    r#"(function(){var v=document.querySelector('video');if(v){if(v.paused){v.play()}else{v.pause()}}})()"#;

const YT_NEXT_JS: &str = r#"(function(){
var b=document.querySelector('ytmusic-player-bar .next-button, .ytp-next-button');
if(b)b.click();})()"#;

const YT_PREV_JS: &str = r#"(function(){
var v=document.querySelector('video');
var m=document.querySelector('ytmusic-player-bar .previous-button');
if(m){m.click();return;}
var p=document.querySelector('.ytp-prev-button');
if(p&&p.offsetParent&&v&&v.currentTime<3){p.click();}else if(v){v.currentTime=0;}})()"#;

fn is_youtube_cond() -> &'static str {
    r#"(u contains "youtube.com/watch" or u contains "music.youtube.com/watch" or u contains "youtube.com/live")"#
}

fn query_browser(app: App) -> Vec<Track> {
    let js = as_str(YT_QUERY_JS);
    let name = app.process_name();
    // NB: inside a browser `tell` block, `tab` means a browser tab, so the
    // separator is `sep` (character id 9) instead.
    let script = match app {
        App::Chromium(_) => format!(
            r#"set sep to character id 9
            tell application "{name}"
                set out to ""
                repeat with w in windows
                    set wid to id of w
                    repeat with t in tabs of w
                        set u to URL of t
                        if {cond} then
                            try
                                set r to execute t javascript "{js}"
                                if r is not missing value and r is not "" then set out to out & wid & sep & (id of t) & sep & r & linefeed
                            on error
                                set out to out & wid & sep & (id of t) & sep & "nojs" & sep & (title of t) & sep & u & linefeed
                            end try
                        end if
                    end repeat
                end repeat
                return out
            end tell"#,
            cond = is_youtube_cond()
        ),
        App::Safari => format!(
            r#"set sep to character id 9
            tell application "Safari"
                set out to ""
                repeat with w in windows
                    set wid to id of w
                    set i to 0
                    repeat with t in tabs of w
                        set i to i + 1
                        set u to URL of t
                        if u is not missing value and {cond} then
                            try
                                set r to do JavaScript "{js}" in t
                                if r is not missing value and r is not "" then set out to out & wid & sep & i & sep & r & linefeed
                            on error
                                set out to out & wid & sep & i & sep & "nojs" & sep & (name of t) & sep & u & linefeed
                            end try
                        end if
                    end repeat
                end repeat
                return out
            end tell"#,
            cond = is_youtube_cond()
        ),
        _ => return vec![],
    };
    let Some(out) = osascript(&script) else {
        return vec![];
    };
    // Without JS we can't ask the <video>, but Core Audio tells us whether the
    // browser is producing sound right now.
    let mut audio_playing: Option<bool> = None;
    let mut browser_audio = || {
        *audio_playing.get_or_insert_with(|| {
            crate::audio::is_playing(match app {
                App::Safari => "com.apple.WebKit.GPU",
                _ => app.bundle_id(),
            })
        })
    };
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let tab = Some((f.first()?.to_string(), f.get(1)?.to_string()));
            if f.get(2) == Some(&"nojs") {
                // No JS access: build what we can from the URL + oEmbed.
                let url = f.get(4).copied().unwrap_or("");
                let id = video_id(url).unwrap_or_else(|| url.to_string());
                let tab_title = f.get(3).copied().unwrap_or("YouTube");
                let tab_title = tab_title
                    .trim_start_matches(|c: char| c == '(' || c.is_ascii_digit() || c == ')')
                    .trim()
                    .trim_end_matches(" - YouTube Music")
                    .trim_end_matches(" - YouTube");
                let (raw_title, channel) =
                    oembed(&id).unwrap_or_else(|| (tab_title.to_string(), String::new()));
                let (title, artist) = if url.contains("music.youtube.com") {
                    (raw_title, channel.trim_end_matches(" - Topic").to_string())
                } else {
                    tidy_youtube_title(&raw_title, &channel)
                };
                return Some(Track {
                    app,
                    playing: browser_audio(),
                    title,
                    artist,
                    artwork_url: Some(thumbnail(&id)),
                    id,
                    position: 0.0,
                    duration: 0.0,
                    tab,
                    full: false,
                    volume: None,
                });
            }
            if f.len() < 10 {
                return None;
            }
            let id = f[8].to_string();
            let is_music = f[9].starts_with("music.");
            let (title, artist) = if is_music {
                (f[5].to_string(), f[6].to_string())
            } else {
                tidy_youtube_title(f[5], f[6])
            };
            let artwork_url = if !is_music && !id.contains('/') {
                Some(thumbnail(&id))
            } else {
                Some(f[7].to_string()).filter(|s| !s.is_empty())
            };
            Some(Track {
                app,
                playing: f[2] == "playing",
                position: num(f[3]),
                duration: num(f[4]),
                title,
                artist,
                artwork_url,
                id,
                tab,
                full: true,
                volume: f.get(10).and_then(|v| num_opt(v)),
            })
        })
        .collect()
}

fn browser_js(t: &Track, js: &str) {
    let Some((wid, tid)) = &t.tab else { return };
    let js = as_str(js);
    let script = match t.app {
        App::Chromium(name) => format!(
            r#"tell application "{name}" to execute (tab id {tid} of window id {wid}) javascript "{js}""#
        ),
        App::Safari => format!(
            r#"tell application "Safari" to do JavaScript "{js}" in tab {tid} of window id {wid}"#
        ),
        _ => return,
    };
    osascript(&script);
}

/// browser_js, but returning the script's result.
fn browser_js_get(t: &Track, js: &str) -> Option<String> {
    let (wid, tid) = t.tab.as_ref()?;
    let js = as_str(js);
    let script = match t.app {
        App::Chromium(name) => format!(
            r#"tell application "{name}" to execute (tab id {tid} of window id {wid}) javascript "{js}""#
        ),
        App::Safari => format!(r#"tell application "Safari" to do JavaScript "{js}" in tab {tid} of window id {wid}"#),
        _ => return None,
    };
    osascript(&script)
}

// ----------------------------------------------------------- queue / playlist

/// The list items on the page, in one place so reading and clicking agree:
/// YouTube Music's queue, a YouTube playlist panel, or YouTube's "up next".
const YT_ITEMS_JS: &str = r#"function __items(){
  if (location.host.indexOf('music.youtube') >= 0)
    return ['q', Array.from(document.querySelectorAll('ytmusic-player-queue #contents > ytmusic-player-queue-item, ytmusic-player-queue #contents > ytmusic-playlist-panel-video-wrapper-renderer ytmusic-player-queue-item'))];
  var p = Array.from(document.querySelectorAll('ytd-playlist-panel-renderer#playlist #items ytd-playlist-panel-video-renderer'));
  if (p.length) return ['p', p];
  return ['n', Array.from(document.querySelectorAll('#secondary ytd-compact-video-renderer, #secondary yt-lockup-view-model')).slice(0, 25)];
}"#;

const YT_QUEUE_JS: &str = r#"(function(){
  var r = __items(), kind = r[0];
  var h = document.querySelector('ytd-playlist-panel-renderer#playlist #header-description .title, ytd-playlist-panel-renderer#playlist h3 yt-formatted-string');
  var out = [kind + '\t' + (kind === 'p' && h ? h.textContent.trim() : '')];
  r[1].forEach(function(e, i){
    var q = function(s){ var x = e.querySelector(s); return x ? x.textContent.trim().replace(/\s+/g, ' ') : ''; };
    var title = q('.song-title') || q('#video-title') || q('h3') || q('a[title]');
    var by = q('.byline') || q('#byline') || q('#channel-name #text') || q('.yt-content-metadata-view-model__metadata-text') || q('.yt-content-metadata-view-model-wiz__metadata-text');
    var cur = e.hasAttribute('selected') || e.getAttribute('play-button-state') === 'playing' || e.getAttribute('play-button-state') === 'paused';
    if (title) out.push([i, title, by, cur ? 1 : 0].join('\t'));
  });
  return out.join('\n');
})()"#;

fn yt_play_js(i: usize) -> String {
    format!(
        r#"(function(){{
  var e = __items()[1][{i}]; if (!e) return;
  var b = e.querySelector('ytmusic-play-button-renderer') || e.querySelector('a#wc-endpoint') || e.querySelector('a#thumbnail') || e.querySelector('a[href*="watch"]') || e.querySelector('a');
  (b || e).click();
}})()"#
    )
}

fn queue(t: &Track) -> Result<Queue, String> {
    match t.app {
        App::Spotify => Err("Spotify ไม่เปิดให้แอปอื่นดูคิวเพลง — ใช้ได้กับ Apple Music และ YouTube".into()),
        App::Music => {
            let out = osascript(
                r#"set sep to character id 9
                tell application "Music"
                    set p to current playlist
                    set n to count of tracks of p
                    set i to 0
                    try
                        set i to index of current track
                    end try
                    set a to i - 3
                    if a < 1 then set a to 1
                    set b to a + 60
                    if b > n then set b to n
                    set out to (name of p) & linefeed
                    repeat with k from a to b
                        set tr to track k of p
                        set out to out & k & sep & (name of tr) & sep & (artist of tr) & sep & ((k = i) as integer) & linefeed
                    end repeat
                    return out
                end tell"#,
            )
            .ok_or("อ่านเพลย์ลิสต์ของ Music ไม่ได้")?;
            let mut lines = out.lines();
            let name = lines.next().unwrap_or("").trim().to_string();
            Ok(Queue { title: format!("เพลย์ลิสต์ · {name}"), items: parse_items(lines) })
        }
        App::Chromium(_) | App::Safari => {
            if !t.full {
                return Err("ต้องเปิด Allow JavaScript from Apple Events ในเบราว์เซอร์ก่อน".into());
            }
            let out = browser_js_get(t, &format!("{YT_ITEMS_JS}
{YT_QUEUE_JS}")).ok_or("อ่านรายการจากหน้าเว็บไม่ได้")?;
            let mut lines = out.lines();
            let (kind, name) = lines.next().unwrap_or("").split_once('\t').unwrap_or(("n", ""));
            let title = match (kind, name.trim()) {
                ("q", _) => "คิวเพลง".to_string(),
                ("p", "") => "เพลย์ลิสต์".to_string(),
                ("p", n) => format!("เพลย์ลิสต์ · {n}"),
                _ => "ถัดไป".to_string(),
            };
            let mut items = parse_items(lines);
            if kind != "q" {
                // "Artist - Song (Official MV)" → Song / Artist, like the player
                for it in &mut items {
                    let (t, a) = tidy_youtube_title(&it.title, &it.artist);
                    it.title = t;
                    it.artist = a;
                }
            }
            Ok(Queue { title, items })
        }
    }
}

fn parse_items<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<QItem> {
    lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 4 && !f[1].trim().is_empty()).then(|| QItem {
                key: f[0].trim().to_string(),
                title: f[1].trim().to_string(),
                artist: f[2].trim().to_string(),
                current: f[3].trim() == "1",
            })
        })
        .collect()
}

fn play_item(t: &Track, k: u32) {
    match t.app {
        App::Music => {
            osascript(&format!(r#"tell application "Music" to play track {k} of current playlist"#));
        }
        App::Chromium(_) | App::Safari => {
            browser_js(t, &format!("{YT_ITEMS_JS}\n{}", yt_play_js(k as usize)));
        }
        App::Spotify => {}
    }
}

fn focus(t: &Track) {
    let name = t.app.process_name();
    let script = match (&t.app, &t.tab) {
        (App::Chromium(_), Some((wid, tid))) => format!(
            r#"tell application "{name}"
                set w to window id {wid}
                set i to 1
                repeat with t in tabs of w
                    if (id of t as text) is "{tid}" then set active tab index of w to i
                    set i to i + 1
                end repeat
                set index of w to 1
                activate
            end tell"#
        ),
        (App::Safari, Some((wid, tid))) => format!(
            r#"tell application "Safari"
                set current tab of window id {wid} to tab {tid} of window id {wid}
                set index of window id {wid} to 1
                activate
            end tell"#
        ),
        _ => format!(r#"tell application "{name}" to activate"#),
    };
    osascript(&script);
}

// ------------------------------------------------------------------- general

pub fn current() -> Option<Track> {
    let mut found: Vec<Track> = Vec::new();
    for app in [App::Spotify, App::Music] {
        // Only talk to an app when it's already running — `tell application`
        // would otherwise launch it (or fail to compile if not installed).
        if is_running(app) {
            if let Some(t) = query_music_app(app) {
                if t.playing {
                    return Some(t);
                }
                found.push(t);
            }
        }
    }
    let browsers = CHROMIUMS.iter().map(|n| App::Chromium(n)).chain([App::Safari]);
    for app in browsers {
        if is_running(app) {
            for t in query_browser(app) {
                if t.playing {
                    return Some(t);
                }
                found.push(t);
            }
        }
    }
    // Nothing known to be playing: prefer a YouTube tab whose state is
    // unknown (likely playing) over a paused music app.
    found.sort_by_key(|t| t.full);
    found.into_iter().next()
}

pub fn fetch_artwork(t: &Track) -> Option<Vec<u8>> {
    match t.app {
        App::Music => {
            let path = std::env::temp_dir().join("notch-artwork");
            let script = format!(
                r#"tell application "Music"
                    try
                        set d to raw data of artwork 1 of current track
                    on error
                        return "none"
                    end try
                end tell
                set f to open for access (POSIX file "{}") with write permission
                set eof f to 0
                write d to f
                close access f
                return "ok""#,
                path.display()
            );
            if osascript(&script)? != "ok" {
                return None;
            }
            std::fs::read(&path).ok()
        }
        _ => {
            let url = t.artwork_url.as_ref()?;
            curl(url).or_else(|| {
                // Not every video has a maxres thumbnail.
                url.contains("maxresdefault")
                    .then(|| curl(&url.replace("maxresdefault", "mqdefault")))
                    .flatten()
            })
        }
    }
}

fn run_cmd(t: &Track, cmd: Cmd) {
    if let Cmd::Focus = cmd {
        return focus(t);
    }
    if let Cmd::PlayItem(key) = cmd {
        return play_item(t, key);
    }
    if let Cmd::FetchQueue = cmd {
        return; // handled in `run` (needs the channel)
    }
    if let Cmd::SetVolume(v) = cmd {
        let n = (v.clamp(0.0, 1.0) * 100.0).round() as i64;
        match t.app {
            App::Spotify | App::Music => {
                osascript(&format!(r#"tell application "{}" to set sound volume to {n}"#, t.app.process_name()));
            }
            App::Chromium(_) | App::Safari => browser_js(
                t,
                &format!(
                    "(function(){{var p=document.getElementById('movie_player');\
                     if(p&&p.setVolume){{if(p.unMute)p.unMute();p.setVolume({n});}}\
                     else{{var v=document.querySelector('video');if(v){{v.muted=false;v.volume={n}/100;}}}}}})()"
                ),
            ),
        }
        return;
    }
    if let Cmd::Seek(secs) = cmd {
        let secs = secs.max(0.0);
        match t.app {
            App::Spotify | App::Music => {
                osascript(&format!(
                    r#"tell application "{}" to set player position to {secs:.2}"#,
                    t.app.process_name()
                ));
            }
            App::Chromium(_) | App::Safari => browser_js(
                t,
                &format!("(function(){{var v=document.querySelector('video');if(v)v.currentTime={secs:.2};}})()"),
            ),
        }
        return;
    }
    match t.app {
        App::Spotify | App::Music => {
            let verb = match cmd {
                Cmd::PlayPause => "playpause",
                Cmd::Next => "next track",
                Cmd::Previous => "previous track",
                _ => unreachable!(),
            };
            osascript(&format!(
                r#"tell application "{}" to {}"#,
                t.app.process_name(),
                verb
            ));
        }
        App::Chromium(_) | App::Safari => {
            let js = match cmd {
                Cmd::PlayPause => YT_PLAYPAUSE_JS,
                Cmd::Next => YT_NEXT_JS,
                Cmd::Previous => YT_PREV_JS,
                _ => unreachable!(),
            };
            browser_js(t, js);
        }
    }
}

/// Background loop: poll every second, or immediately after a command.
pub fn run(tx: Sender<Msg>, cmds: Receiver<Cmd>) {
    let mut last: Option<Track> = None;
    let mut last_art_key: Option<String> = None;
    let mut misses = 0;
    loop {
        let mut now = current();
        // A single empty poll (song change, ad, slow osascript) shouldn't make
        // the island shrink and grow back: require 3 in a row.
        if now.is_none() && last.is_some() {
            misses += 1;
            if misses < 3 {
                now = last.clone();
            }
        } else {
            misses = 0;
        }
        if now != last {
            let key = now
                .as_ref()
                .map(|t| format!("{:?}{}{:?}", t.app, t.id, t.artwork_url));
            let art_changed = key != last_art_key;
            if tx.send(Msg::Track(now.clone())).is_err() {
                return;
            }
            if art_changed {
                let art = now.as_ref().and_then(fetch_artwork);
                let _ = tx.send(Msg::Artwork(art));
                last_art_key = key;
            }
            last = now;
        }
        match cmds.recv_timeout(Duration::from_secs(1)) {
            Ok(first) => {
                let mut cmds_now = vec![first];
                while let Ok(c) = cmds.try_recv() {
                    cmds_now.push(c);
                }
                let last_vol = cmds_now.iter().rposition(|c| matches!(c, Cmd::SetVolume(_)));
                let last_seek = cmds_now.iter().rposition(|c| matches!(c, Cmd::Seek(_)));
                if let Some(t) = &last {
                    for (i, cmd) in cmds_now.into_iter().enumerate() {
                        let stale = (matches!(cmd, Cmd::SetVolume(_)) && Some(i) != last_vol)
                            || (matches!(cmd, Cmd::Seek(_)) && Some(i) != last_seek);
                        if matches!(cmd, Cmd::FetchQueue) {
                            let _ = tx.send(Msg::Queue(queue(t)));
                        } else if !stale {
                            run_cmd(t, cmd);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_titles() {
        assert_eq!(
            tidy_youtube_title("IVE, David Guetta - Supernova Love Official Music Video", "IVE"),
            ("Supernova Love".into(), "IVE, David Guetta".into())
        );
        assert_eq!(
            tidy_youtube_title("IVE 아이브 'REBEL HEART' MV", "STARSHIP"),
            ("REBEL HEART".into(), "IVE 아이브".into())
        );
        assert_eq!(
            tidy_youtube_title("Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)", "Rick Astley"),
            ("Never Gonna Give You Up".into(), "Rick Astley".into())
        );
        assert_eq!(
            tidy_youtube_title("เพลงไทย [Official MV]", "ค่ายเพลง"),
            ("เพลงไทย".into(), "ค่ายเพลง".into())
        );
    }

    #[test]
    fn ids() {
        assert_eq!(
            video_id("https://www.youtube.com/watch?v=fyk6vjwI3wc&list=RD").as_deref(),
            Some("fyk6vjwI3wc")
        );
    }
}

#[cfg(test)]
mod live_tests {
    #[test]
    #[ignore] // needs a running player
    fn live_queue() {
        let t = super::current().expect("nothing playing");
        match super::queue(&t) {
            Ok(q) => {
                println!("{} ({} items)", q.title, q.items.len());
                for i in q.items.iter().filter(|i| i.current).chain(q.items.iter().take(2)) {
                    println!("  {} {} — {} {}", i.key, i.title, i.artist, if i.current { "◀" } else { "" });
                }
            }
            Err(e) => println!("error: {e}"),
        }
    }
}
