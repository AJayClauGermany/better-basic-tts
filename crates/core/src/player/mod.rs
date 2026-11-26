//! The transport: play / pause / stop / previous / next / slower / faster.
//!
//! [`Player`] is a cheap handle. All state lives on a control thread that
//! owns the audio output; synthesis happens on two helper threads so that a
//! slow engine never blocks the controls. Sentences are synthesised one at a
//! time and the next few are prepared ahead, which keeps start-up latency at
//! "one sentence" no matter how long the text is.

pub mod output;

use crate::engine::{Audio, Engine, SynthRequest};
use crate::settings::{clamp_rate, RoleStyles, Settings, RATE_STEP};
use crate::text::{Role, Utterance};
use output::AudioOutput;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many sentences are synthesised ahead of the one being played.
const PREFETCH: usize = 2;
/// "Previous" restarts the current sentence when more than this much of it
/// has been heard, and goes to the one before otherwise.
const RESTART_THRESHOLD_SECS: f64 = 1.5;
/// When the speed changes mid-sentence, back up a little so no word is lost.
const RATE_CHANGE_BACKOFF_SECS: f64 = 0.3;
/// Release the audio device after this long without playback.
const RELEASE_AFTER: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_millis(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Nothing is being read. There may still be text loaded (after "stop"),
    /// in which case "play" starts it from the beginning.
    #[default]
    Idle,
    /// Waiting for the engine to produce the current sentence.
    Loading,
    Playing,
    Paused,
}

/// What the UI needs to draw itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerSnapshot {
    pub status: Status,
    /// Index of the current sentence.
    pub index: usize,
    /// Number of sentences loaded.
    pub total: usize,
    pub rate: f32,
    /// The current sentence as written.
    pub current: Option<String>,
    pub role: Option<Role>,
    /// Where the text came from ("Selection", "Clipboard", an app name, ...).
    pub title: String,
    /// The most recent problem, cleared when playback next starts.
    pub error: Option<String>,
}

impl PlayerSnapshot {
    pub fn is_active(&self) -> bool {
        self.status != Status::Idle
    }
}

/// Everything the player needs to know about *how* to speak.
#[derive(Clone)]
pub struct PlayerConfig {
    pub engine: Option<Arc<dyn Engine>>,
    pub voice: Option<String>,
    pub volume: f32,
    pub sentence_pause_ms: u32,
    pub roles: RoleStyles,
}

impl PlayerConfig {
    pub fn from_settings(settings: &Settings, engine: Option<Arc<dyn Engine>>) -> Self {
        Self {
            engine,
            voice: (!settings.voice.is_empty()).then(|| settings.voice.clone()),
            volume: settings.volume,
            sentence_pause_ms: settings.sentence_pause_ms,
            roles: settings.roles.clone(),
        }
    }
}

enum Command {
    Speak { utterances: Vec<Utterance>, title: String },
    TogglePause,
    Pause,
    Stop,
    Next,
    Previous,
    SetRate(f32),
    NudgeRate(i32),
    Configure(Box<PlayerConfig>),
    Shutdown,
}

enum Msg {
    Command(Command),
    Synthesized {
        generation: u64,
        key: Key,
        result: Result<Arc<Audio>, String>,
    },
}

/// (sentence index, rate in thousandths)
type Key = (usize, u32);

fn rate_key(rate: f32) -> u32 {
    (rate * 1000.0).round() as u32
}

type Notify = Arc<dyn Fn() + Send + Sync>;

/// Handle to the playback machinery. Clone freely.
#[derive(Clone)]
pub struct Player {
    tx: flume::Sender<Msg>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
    _guard: Arc<ShutdownGuard>,
}

struct ShutdownGuard {
    tx: flume::Sender<Msg>,
    jobs: Arc<JobQueue>,
}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Command(Command::Shutdown));
        self.jobs.shutdown();
    }
}

impl Player {
    /// Start the player.
    ///
    /// * `make_output` runs on the control thread (audio devices are not
    ///   `Send` on every platform).
    /// * `on_change` is called (from the control thread) whenever
    ///   [`Player::snapshot`] would return something new.
    pub fn new(
        make_output: impl FnOnce() -> Box<dyn AudioOutput> + Send + 'static,
        config: PlayerConfig,
        rate: f32,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = flume::unbounded::<Msg>();
        let snapshot = Arc::new(Mutex::new(PlayerSnapshot {
            rate: clamp_rate(rate),
            ..Default::default()
        }));
        let jobs = Arc::new(JobQueue::default());
        for n in 0..2 {
            let jobs = jobs.clone();
            let tx = tx.clone();
            std::thread::Builder::new()
                .name(format!("bbtts-synth-{n}"))
                .spawn(move || synth_thread(jobs, tx))
                .expect("could not start synthesis thread");
        }
        {
            let jobs = jobs.clone();
            let snapshot = snapshot.clone();
            let notify: Notify = Arc::new(on_change);
            std::thread::Builder::new()
                .name("bbtts-player".into())
                .spawn(move || {
                    let mut worker = Worker {
                        out: make_output(),
                        cfg: config,
                        playlist: Vec::new(),
                        title: String::new(),
                        idx: 0,
                        active: false,
                        paused: false,
                        rate: clamp_rate(rate),
                        loaded: None,
                        resume_fraction: 0.0,
                        cache: HashMap::new(),
                        generation: 0,
                        error: None,
                        jobs,
                        snapshot,
                        notify,
                        idle_since: Some(Instant::now()),
                        released: false,
                    };
                    worker.run(rx);
                })
                .expect("could not start player thread");
        }
        Self {
            tx: tx.clone(),
            snapshot,
            _guard: Arc::new(ShutdownGuard { tx, jobs }),
        }
    }

    fn send(&self, command: Command) {
        let _ = self.tx.send(Msg::Command(command));
    }

    /// Replace the loaded text and start reading it.
    pub fn speak(&self, utterances: Vec<Utterance>, title: impl Into<String>) {
        self.send(Command::Speak {
            utterances,
            title: title.into(),
        });
    }

    /// Play ⇄ pause. When idle with text loaded, starts from the beginning.
    pub fn toggle_pause(&self) {
        self.send(Command::TogglePause);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    /// Skip to the next sentence.
    pub fn next(&self) {
        self.send(Command::Next);
    }

    /// Restart the current sentence, or go to the previous one if the
    /// current one has only just begun.
    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    pub fn faster(&self) {
        self.send(Command::NudgeRate(1));
    }

    pub fn slower(&self) {
        self.send(Command::NudgeRate(-1));
    }

    pub fn set_rate(&self, rate: f32) {
        self.send(Command::SetRate(rate));
    }

    /// Change engine / voice / role styles. Takes effect immediately, also
    /// in the middle of a sentence.
    pub fn configure(&self, config: PlayerConfig) {
        self.send(Command::Configure(Box::new(config)));
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Synthesis jobs
// ---------------------------------------------------------------------------

struct Job {
    generation: u64,
    key: Key,
    engine: Arc<dyn Engine>,
    request: SynthRequest,
    pause_after_ms: u32,
}

#[derive(Default)]
struct JobQueue {
    state: Mutex<JobState>,
    wake: Condvar,
}

#[derive(Default)]
struct JobState {
    pending: VecDeque<Job>,
    running: HashSet<(u64, Key)>,
    shutdown: bool,
}

impl JobQueue {
    /// Replace the pending jobs. Jobs already running are left alone and not
    /// queued a second time.
    fn set(&self, jobs: Vec<Job>) {
        let mut state = self.state.lock().unwrap();
        state.pending = jobs
            .into_iter()
            .filter(|j| !state.running.contains(&(j.generation, j.key)))
            .collect();
        self.wake.notify_all();
    }

    fn take(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.shutdown {
                return None;
            }
            if let Some(job) = state.pending.pop_front() {
                state.running.insert((job.generation, job.key));
                return Some(job);
            }
            state = self.wake.wait(state).unwrap();
        }
    }

    fn done(&self, generation: u64, key: Key) {
        self.state.lock().unwrap().running.remove(&(generation, key));
    }

    fn shutdown(&self) {
        self.state.lock().unwrap().shutdown = true;
        self.wake.notify_all();
    }
}

fn synth_thread(jobs: Arc<JobQueue>, tx: flume::Sender<Msg>) {
    while let Some(job) = jobs.take() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            job.engine.synthesize(&job.request)
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the speech engine crashed")))
        .map(|mut audio| {
            audio.trim_silence(25);
            audio.append_silence(job.pause_after_ms);
            Arc::new(audio)
        })
        .map_err(|e| format!("{e:#}"));
        // The job stays marked as running until the control thread has
        // stored the result; otherwise it could be queued a second time in
        // the window between "finished" and "cached".
        if tx
            .send(Msg::Synthesized {
                generation: job.generation,
                key: job.key,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Control thread
// ---------------------------------------------------------------------------

struct Worker {
    out: Box<dyn AudioOutput>,
    cfg: PlayerConfig,
    playlist: Vec<Utterance>,
    title: String,
    idx: usize,
    /// Reading is in progress (possibly paused, possibly still loading).
    active: bool,
    paused: bool,
    rate: f32,
    /// The audio currently handed to the output.
    loaded: Option<(Key, Arc<Audio>)>,
    /// Where in the current sentence to start once its audio is ready
    /// (0.0 ..= 1.0); non-zero after a speed or voice change mid-sentence.
    resume_fraction: f64,
    cache: HashMap<Key, Arc<Audio>>,
    /// Bumped whenever cached audio becomes invalid (new text, new voice).
    generation: u64,
    error: Option<String>,
    jobs: Arc<JobQueue>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
    notify: Notify,
    idle_since: Option<Instant>,
    /// The audio device has been released after a period of inactivity.
    released: bool,
}

impl Worker {
    fn run(&mut self, rx: flume::Receiver<Msg>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Msg::Command(Command::Shutdown)) | Err(flume::RecvTimeoutError::Disconnected) => {
                    self.out.stop();
                    self.jobs.shutdown();
                    return;
                }
                Ok(Msg::Command(command)) => self.handle(command),
                Ok(Msg::Synthesized {
                    generation,
                    key,
                    result,
                }) => self.synthesized(generation, key, result),
                Err(flume::RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            self.publish();
        }
    }

    fn key(&self) -> Key {
        (self.idx, rate_key(self.rate))
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Speak { utterances, title } => {
                self.invalidate();
                self.playlist = utterances;
                self.title = title;
                self.idx = 0;
                self.error = None;
                if self.playlist.is_empty() {
                    self.halt();
                    self.error = Some("There is no text to read.".into());
                } else {
                    self.begin();
                }
            }
            Command::TogglePause => {
                if !self.active {
                    if !self.playlist.is_empty() {
                        self.idx = 0;
                        self.error = None;
                        self.begin();
                    }
                } else if self.paused {
                    self.paused = false;
                    if self.loaded.is_some() {
                        self.out.resume();
                    }
                } else {
                    self.paused = true;
                    self.out.pause();
                }
            }
            Command::Pause => {
                if self.active && !self.paused {
                    self.paused = true;
                    self.out.pause();
                }
            }
            Command::Stop => {
                self.halt();
                self.idx = 0;
            }
            Command::Next => {
                if self.active {
                    if self.idx + 1 < self.playlist.len() {
                        self.idx += 1;
                        self.start_current(0.0);
                    } else {
                        self.halt();
                        self.idx = 0;
                    }
                }
            }
            Command::Previous => {
                if self.active {
                    let heard = self
                        .loaded
                        .as_ref()
                        .map(|(_, a)| self.out.position() as f64 / a.sample_rate.max(1) as f64)
                        .unwrap_or(0.0);
                    if self.idx > 0 && heard < RESTART_THRESHOLD_SECS {
                        self.idx -= 1;
                    }
                    self.start_current(0.0);
                }
            }
            Command::NudgeRate(steps) => self.set_rate(self.rate + steps as f32 * RATE_STEP),
            Command::SetRate(rate) => self.set_rate(rate),
            Command::Configure(config) => {
                let fraction = self.current_fraction();
                self.cfg = *config;
                self.invalidate();
                if self.active {
                    self.start_current(fraction);
                }
            }
            Command::Shutdown => {}
        }
    }

    fn set_rate(&mut self, rate: f32) {
        let rate = clamp_rate(rate);
        if rate_key(rate) == rate_key(self.rate) {
            return;
        }
        let fraction = self.current_fraction();
        self.rate = rate;
        if self.active {
            self.start_current(fraction);
        }
    }

    /// How far through the current sentence we are, 0.0 ..= 1.0.
    fn current_fraction(&self) -> f64 {
        match &self.loaded {
            Some((_, audio)) if !audio.samples.is_empty() => {
                (self.out.position() as f64 / audio.samples.len() as f64).clamp(0.0, 1.0)
            }
            // Still waiting for audio: keep whatever was requested before.
            _ => self.resume_fraction,
        }
    }

    fn invalidate(&mut self) {
        self.generation += 1;
        self.cache.clear();
    }

    fn begin(&mut self) {
        self.active = true;
        self.paused = false;
        self.start_current(0.0);
    }

    fn halt(&mut self) {
        self.active = false;
        self.paused = false;
        self.loaded = None;
        self.resume_fraction = 0.0;
        self.out.stop();
        self.jobs.set(Vec::new());
    }

    /// Play sentence `self.idx` at the current rate, from `fraction` of its
    /// length — now if its audio is cached, otherwise as soon as it arrives.
    fn start_current(&mut self, fraction: f64) {
        let key = self.key();
        self.resume_fraction = fraction;
        match self.cache.get(&key).cloned() {
            Some(audio) => {
                let mut start = (fraction * audio.samples.len() as f64) as usize;
                if fraction > 0.0 {
                    let backoff = (RATE_CHANGE_BACKOFF_SECS * audio.sample_rate as f64) as usize;
                    start = start.saturating_sub(backoff);
                }
                match self.out.play(audio.clone(), start) {
                    Ok(()) => {
                        if self.paused {
                            self.out.pause();
                        }
                        self.loaded = Some((key, audio));
                        self.resume_fraction = 0.0;
                    }
                    Err(err) => {
                        self.halt();
                        self.error = Some(format!("{err:#}"));
                        return;
                    }
                }
            }
            None => {
                self.out.stop();
                self.loaded = None;
            }
        }
        self.schedule();
    }

    /// Queue synthesis for the current sentence and the next few.
    fn schedule(&mut self) {
        let rk = rate_key(self.rate);
        // Forget audio we will not need again soon.
        let idx = self.idx;
        self.cache
            .retain(|&(i, r), _| r == rk && i + 2 >= idx && i <= idx + PREFETCH + 1);

        let Some(engine) = self.cfg.engine.clone() else {
            if self.active {
                self.halt();
                self.error = Some(crate::engine::EngineRegistry::missing_engine_help().into());
            }
            return;
        };
        let mut jobs = Vec::new();
        if self.active {
            for i in idx..(idx + 1 + PREFETCH).min(self.playlist.len()) {
                let key = (i, rk);
                if self.cache.contains_key(&key) {
                    continue;
                }
                let utterance = &self.playlist[i];
                let style = self.cfg.roles.get(utterance.role);
                let starts_block = i == 0 || self.playlist[i - 1].ends_block;
                let text = if starts_block && !style.announce.trim().is_empty() {
                    format!("{} {}", style.announce.trim(), utterance.spoken)
                } else {
                    utterance.spoken.clone()
                };
                jobs.push(Job {
                    generation: self.generation,
                    key,
                    engine: engine.clone(),
                    request: SynthRequest {
                        text,
                        voice: if style.voice.is_empty() {
                            self.cfg.voice.clone()
                        } else {
                            Some(style.voice.clone())
                        },
                        rate: self.rate * style.rate,
                        pitch: style.pitch,
                        volume: (self.cfg.volume * style.volume).clamp(0.0, 1.0),
                    },
                    pause_after_ms: if utterance.ends_block {
                        style.pause_after_ms
                    } else {
                        self.cfg.sentence_pause_ms
                    },
                });
            }
        }
        self.jobs.set(jobs);
    }

    fn synthesized(&mut self, generation: u64, key: Key, result: Result<Arc<Audio>, String>) {
        if generation != self.generation {
            self.jobs.done(generation, key);
            return;
        }
        if let Ok(audio) = &result {
            self.cache.insert(key, audio.clone());
        }
        self.jobs.done(generation, key);
        let wanted = self.active && self.loaded.is_none() && key == self.key();
        match result {
            Ok(_) => {
                if wanted {
                    self.start_current(self.resume_fraction);
                } else {
                    self.schedule();
                }
            }
            Err(message) => {
                // Errors for sentences further ahead surface when (if) we
                // actually get there.
                if wanted {
                    self.halt();
                    self.error = Some(message);
                }
            }
        }
    }

    fn tick(&mut self) {
        if self.active && !self.paused && self.loaded.is_some() && self.out.finished() {
            if self.idx + 1 < self.playlist.len() {
                self.idx += 1;
                self.start_current(0.0);
            } else {
                self.halt();
                self.idx = 0;
            }
        }
        // Let the audio device sleep when we are not using it.
        if self.active && !self.paused {
            self.idle_since = None;
            self.released = false;
        } else {
            let since = *self.idle_since.get_or_insert_with(Instant::now);
            if !self.active && !self.released && since.elapsed() > RELEASE_AFTER {
                self.out.release();
                self.released = true;
            }
        }
    }

    fn publish(&mut self) {
        let status = if !self.active {
            Status::Idle
        } else if self.paused {
            Status::Paused
        } else if self.loaded.is_none() {
            Status::Loading
        } else {
            Status::Playing
        };
        let current = self.playlist.get(self.idx);
        let snapshot = PlayerSnapshot {
            status,
            index: self.idx,
            total: self.playlist.len(),
            rate: self.rate,
            current: current.map(|u| u.display.clone()),
            role: current.map(|u| u.role),
            title: self.title.clone(),
            error: self.error.clone(),
        };
        let changed = {
            let mut shared = self.snapshot.lock().unwrap();
            if *shared != snapshot {
                *shared = snapshot;
                true
            } else {
                false
            }
        };
        if changed {
            (self.notify)();
        }
    }
}

#[cfg(test)]
mod tests;
