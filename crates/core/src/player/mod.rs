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

/// How many sentences are synthesised ahead of the one being played. Deep
/// enough that a few short sentences in a row, or an engine that is only a
/// little faster than real time, do not make the reading stall.
const PREFETCH: usize = 6;
/// Synthesis threads. More than one lets the sentence that is needed *now*
/// overtake look-ahead work that is already running.
const SYNTH_THREADS: usize = 3;
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
    Speak {
        utterances: Vec<Utterance>,
        title: String,
    },
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
        for n in 0..SYNTH_THREADS {
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
                        queued: None,
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

/// One call to the engine: one sentence, or several consecutive ones that
/// share voice and speed for engines that are cheaper per sentence that way.
struct Job {
    generation: u64,
    engine: Arc<dyn Engine>,
    /// Voice, rate, pitch and volume; the text is in `items`.
    params: SynthRequest,
    items: Vec<JobItem>,
}

struct JobItem {
    key: Key,
    text: String,
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
    /// Replace the pending jobs. `plan` is told which sentences are being
    /// synthesised right now so it does not ask for them a second time.
    fn plan(&self, plan: impl FnOnce(&HashSet<(u64, Key)>) -> Vec<Job>) {
        let mut state = self.state.lock().unwrap();
        state.pending = plan(&state.running).into();
        self.wake.notify_all();
    }

    fn clear(&self) {
        self.state.lock().unwrap().pending.clear();
    }

    fn take(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.shutdown {
                return None;
            }
            if let Some(job) = state.pending.pop_front() {
                for item in &job.items {
                    state.running.insert((job.generation, item.key));
                }
                return Some(job);
            }
            state = self.wake.wait(state).unwrap();
        }
    }

    fn done(&self, generation: u64, key: Key) {
        self.state
            .lock()
            .unwrap()
            .running
            .remove(&(generation, key));
    }

    fn shutdown(&self) {
        self.state.lock().unwrap().shutdown = true;
        self.wake.notify_all();
    }
}

fn synth_thread(jobs: Arc<JobQueue>, tx: flume::Sender<Msg>) {
    while let Some(job) = jobs.take() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let [item] = job.items.as_slice() {
                job.engine
                    .synthesize(&SynthRequest {
                        text: item.text.clone(),
                        ..job.params.clone()
                    })
                    .map(|audio| vec![audio])
            } else {
                let texts: Vec<String> = job.items.iter().map(|i| i.text.clone()).collect();
                job.engine.synthesize_batch(&texts, &job.params)
            }
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the speech engine crashed")))
        .and_then(|audios| {
            if audios.len() == job.items.len() {
                Ok(audios)
            } else {
                Err(anyhow::anyhow!(
                    "the speech engine returned {} pieces of audio for {} sentences",
                    audios.len(),
                    job.items.len()
                ))
            }
        });
        let results: Vec<Result<Arc<Audio>, String>> = match outcome {
            Ok(audios) => audios
                .into_iter()
                .zip(&job.items)
                .map(|(mut audio, item)| {
                    audio.trim_silence(25);
                    audio.append_silence(item.pause_after_ms);
                    Ok(Arc::new(audio))
                })
                .collect(),
            Err(e) => {
                let message = format!("{e:#}");
                job.items.iter().map(|_| Err(message.clone())).collect()
            }
        };
        // The sentences stay marked as running until the control thread has
        // stored the results; otherwise they could be queued a second time
        // in the window between "finished" and "cached".
        for (item, result) in job.items.iter().zip(results) {
            if tx
                .send(Msg::Synthesized {
                    generation: job.generation,
                    key: item.key,
                    result,
                })
                .is_err()
            {
                return;
            }
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
    /// The audio currently handed to the output, with the generation it
    /// was made for. After a speed or voice change this can be audio for the
    /// *old* settings that keeps playing until the new audio is ready.
    loaded: Option<(u64, Key, Arc<Audio>)>,
    /// The next sentence, already lined up in the output so that it follows
    /// the current one without a seam.
    queued: Option<(u64, Key, Arc<Audio>)>,
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
                Ok(Msg::Command(Command::Shutdown))
                | Err(flume::RecvTimeoutError::Disconnected) => {
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
            self.line_up_next();
            self.publish();
        }
    }

    fn key(&self) -> Key {
        (self.idx, rate_key(self.rate))
    }

    /// The output has moved on to the sentence we lined up: follow it.
    fn follow_output(&mut self) {
        if let Some((generation, key, audio)) = self.queued.take() {
            self.idx = key.0;
            self.loaded = Some((generation, key, audio));
            self.resume_fraction = 0.0;
            self.schedule();
        }
    }

    /// Take back the lined-up sentence before changing course. If the output
    /// got to it first, we are simply one sentence further along.
    fn settle_queue(&mut self) {
        if self.queued.is_none() {
            return;
        }
        if self.out.clear_next() {
            self.queued = None;
        } else {
            self.out.advanced();
            self.follow_output();
        }
    }

    /// Hand the output the sentence after the current one, if it is ready,
    /// so the transition happens inside the audio stream.
    fn line_up_next(&mut self) {
        if !self.active || self.queued.is_some() {
            return;
        }
        let Some((generation, key, audio)) = &self.loaded else {
            return;
        };
        // Only behind audio that reflects the current speed and voice.
        if *generation != self.generation || *key != self.key() {
            return;
        }
        let next_key = (self.idx + 1, key.1);
        let Some(next) = self.cache.get(&next_key) else {
            return;
        };
        if next.sample_rate == audio.sample_rate && self.out.queue_next(next.clone()) {
            self.queued = Some((self.generation, next_key, next.clone()));
        }
    }

    fn handle(&mut self, command: Command) {
        // Every command may change what should come next.
        self.settle_queue();
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
                        self.start_current(0.0, true);
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
                        .map(|(_, _, a)| self.out.position() as f64 / a.sample_rate.max(1) as f64)
                        .unwrap_or(0.0);
                    if self.idx > 0 && heard < RESTART_THRESHOLD_SECS {
                        self.idx -= 1;
                    }
                    self.start_current(0.0, true);
                }
            }
            Command::NudgeRate(steps) => {
                // To the next step of the grid in that direction, so a speed
                // typed into the settings (say 1.1) joins the grid again.
                let grid = self.rate / RATE_STEP;
                let from = if steps > 0 {
                    (grid + 1e-3).floor()
                } else {
                    (grid - 1e-3).ceil()
                };
                self.set_rate((from + steps as f32) * RATE_STEP);
            }
            Command::SetRate(rate) => self.set_rate(rate),
            Command::Configure(config) => {
                let fraction = self.current_fraction();
                self.cfg = *config;
                self.invalidate();
                if self.active {
                    self.start_current(fraction, false);
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
            self.start_current(fraction, false);
        }
    }

    /// How far through the current sentence we are, 0.0 ..= 1.0.
    fn current_fraction(&self) -> f64 {
        match &self.loaded {
            Some((_, _, audio)) if !audio.samples.is_empty() => {
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
        self.start_current(0.0, true);
    }

    fn halt(&mut self) {
        self.active = false;
        self.paused = false;
        self.loaded = None;
        self.queued = None;
        self.resume_fraction = 0.0;
        self.out.stop();
        self.jobs.clear();
    }

    /// Play sentence `self.idx` at the current rate, from `fraction` of its
    /// length — now if its audio is cached, otherwise as soon as it arrives.
    ///
    /// `interrupt` says what to do in the meantime: skipping to another
    /// sentence silences the output at once, whereas after a speed or voice
    /// change the old audio keeps playing until the new one can take over, so
    /// a slow engine does not cause a gap.
    fn start_current(&mut self, fraction: f64, interrupt: bool) {
        let key = self.key();
        self.resume_fraction = fraction;
        if self.queued.take().is_some() {
            self.out.clear_next();
        }
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
                        self.loaded = Some((self.generation, key, audio));
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
                let same_sentence = self
                    .loaded
                    .as_ref()
                    .is_some_and(|(_, (i, _), _)| *i == self.idx);
                if interrupt || !same_sentence {
                    self.out.stop();
                    self.loaded = None;
                }
            }
        }
        self.schedule();
    }

    /// Queue synthesis for the current sentence and the next few.
    fn schedule(&mut self) {
        let rk = rate_key(self.rate);
        let idx = self.idx;
        // Engines that take several sentences per call are fed in batches,
        // which needs a longer view ahead to fill them.
        let batch_size = self
            .cfg
            .engine
            .as_ref()
            .map(|e| e.batch_size().max(1))
            .unwrap_or(1);
        let lookahead = PREFETCH.max(2 * batch_size + 1);
        // Forget audio we will not need again soon.
        self.cache
            .retain(|&(i, r), _| r == rk && i + 2 >= idx && i <= idx + lookahead + 1);

        let Some(engine) = self.cfg.engine.clone() else {
            if self.active {
                self.halt();
                self.error = Some(crate::engine::EngineRegistry::missing_engine_help().into());
            }
            return;
        };
        // What we would like to have, in reading order.
        let mut wanted: Vec<(SynthRequest, JobItem)> = Vec::new();
        if self.active {
            for i in idx..(idx + 1 + lookahead).min(self.playlist.len()) {
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
                let params = SynthRequest {
                    text: String::new(),
                    voice: if style.voice.is_empty() {
                        self.cfg.voice.clone()
                    } else {
                        Some(style.voice.clone())
                    },
                    rate: self.rate * style.rate,
                    pitch: style.pitch,
                    volume: (self.cfg.volume * style.volume).clamp(0.0, 1.0),
                };
                let pause_after_ms = if utterance.ends_block {
                    style.pause_after_ms
                } else {
                    self.cfg.sentence_pause_ms
                };
                wanted.push((
                    params,
                    JobItem {
                        key,
                        text,
                        pause_after_ms,
                    },
                ));
            }
        }

        let generation = self.generation;
        let window_end = (idx + 1 + lookahead).min(self.playlist.len());
        let more_text_follows = window_end < self.playlist.len();
        self.jobs.plan(|running| {
            let mut jobs: Vec<Job> = Vec::new();
            for (params, item) in wanted {
                if running.contains(&(generation, item.key)) {
                    continue;
                }
                // The sentence that is due now and the one after it go alone,
                // so that reading starts (and continues) as soon as possible;
                // what follows may share calls.
                let urgent = item.key.0 <= idx + 1;
                let joins_previous = jobs.last().is_some_and(|job| {
                    let last = job.items.last().expect("jobs are never empty");
                    !urgent
                        && last.key.0 > idx + 1
                        && last.key.0 + 1 == item.key.0
                        && job.items.len() < batch_size
                        && job.params == params
                });
                if joins_previous {
                    jobs.last_mut().expect("checked above").items.push(item);
                } else {
                    jobs.push(Job {
                        generation,
                        engine: engine.clone(),
                        params,
                        items: vec![item],
                    });
                }
            }
            // A batch at the far edge of the window that is not full yet
            // will fill up as reading moves on: wait for it rather than
            // spend a whole call on a sentence or two.
            if let Some(job) = jobs.last() {
                let last = job.items.last().expect("jobs are never empty").key.0;
                let can_grow = more_text_follows && last + 1 == window_end;
                if last > idx + 1 && job.items.len() < batch_size && can_grow {
                    jobs.pop();
                }
            }
            jobs
        });
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
        // Is the output already playing exactly this (sentence, rate, voice)?
        let up_to_date = self
            .loaded
            .as_ref()
            .is_some_and(|(g, k, _)| *g == self.generation && *k == self.key());
        let wanted = self.active && key == self.key() && !up_to_date;
        match result {
            Ok(_) => {
                if wanted {
                    // If older audio of this sentence is still playing, take
                    // over from where it is now.
                    self.start_current(self.current_fraction(), true);
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
        // The seamless path: the output already carried on by itself.
        if self.queued.is_some() && self.out.advanced() {
            self.follow_output();
        }
        if self.active && !self.paused && self.loaded.is_some() && self.out.finished() {
            if self.idx + 1 < self.playlist.len() {
                self.idx += 1;
                self.start_current(0.0, true);
                if self.loaded.is_none() {
                    log::info!(
                        "sentence {} was not ready in time; waiting for the speech engine",
                        self.idx + 1
                    );
                }
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
