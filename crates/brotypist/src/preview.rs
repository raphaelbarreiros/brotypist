//! Ephemeral inference for a caller-owned Welcome field. No native capture,
//! history, clipboard, screen references, settings writes or external insertion.
use crate::{config::Config, core::{Snapshot, SnapshotKey, SuggestionState}, features,
    process_client::WorkerClient, providers::GenerationOptions, runtime::{Clock, MonotonicClock, Worker}};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

struct Request { id: u64, key: SnapshotKey, generation: u64, proceeded: bool }
/// Calls are serialized by the widget owner. Returned acceptance text is only a
/// proposal; update must confirm exact native-widget readback before accounting.
pub struct PreviewSession {
    pub config: Config, worker: Box<dyn Worker>, clock: Box<dyn Clock>, snapshot: Snapshot,
    suggestion: SuggestionState, request: Option<Request>, attempted: Option<SnapshotKey>,
    expected: Option<Snapshot>, changed: Instant, accepted: u64, error: String,
    active: bool, owner_released: bool, closed: bool, stopped: bool,
}
impl PreviewSession {
    pub fn new(config: Config) -> Result<Self> { Self::with_worker(config, Box::new(WorkerClient::new()?), Box::new(MonotonicClock)) }
    pub fn with_worker(mut config: Config, mut worker: Box<dyn Worker>, clock: Box<dyn Clock>) -> Result<Self> {
        config.validate()?; worker.set_idle_timeout(config.idle_release_seconds); let changed = clock.now();
        Ok(Self { config, worker, clock, snapshot: Snapshot { app_id: "brotypist.preview".into(), field_id: "welcome-try-it".into(), ..Default::default() },
            suggestion: SuggestionState::new(), request: None, attempted: None, expected: None, changed, accepted: 0, error: String::new(),
            active: true, owner_released: false, closed: false, stopped: false })
    }
    pub fn set_owner_released(&mut self, released: bool) {
        self.owner_released = released;
        if !released { let _ = self.worker.cancel(); self.request = None; self.suggestion.invalidate(); self.expected = None; }
    }
    /// Focus loss and marked-text/IME composition must call set_active(false).
    pub fn set_active(&mut self, active: bool) {
        if self.active == active { return; } self.active = active;
        let _ = self.worker.cancel(); self.request = None; self.expected = None; self.suggestion.invalidate(); self.attempted = None; self.changed = self.clock.now();
    }
    pub fn update(&mut self, text: &str, caret: usize, selection_length: usize) -> Result<()> {
        if self.closed { return Ok(()); }
        let count = text.chars().count();
        ensure!(caret <= count && selection_length <= count-caret, "Preview cursor must describe the current owned document.");
        let mut current = self.snapshot.clone(); current.text = text.into(); current.caret = caret; current.selection_length = selection_length;
        if current.same_key(&self.snapshot) { return Ok(()); }
        self.worker.cancel()?; self.request = None;
        let confirmed = self.expected.as_ref().is_some_and(|expected| expected.same_key(&current) && expected.same_authority(&current));
        if confirmed { self.accepted += 1; self.suggestion.snapshot = Some(current.clone()); }
        else if !self.suggestion.advance(&current) { self.suggestion.invalidate(); }
        self.expected = None; self.snapshot = current;
        self.attempted = (!self.suggestion.text.is_empty()).then(|| self.snapshot.key()); self.changed = self.clock.now(); self.error.clear(); Ok(())
    }
    fn eligible(&self) -> bool {
        self.active && self.snapshot.eligible(self.config.min_chars as usize)
            && (self.config.complete_inside_text || self.snapshot.suffix().is_empty())
            && (self.config.completion_length != "word" || features::mid_word(self.snapshot.prefix()))
    }
    pub fn poll(&mut self) -> Value {
        if self.closed {
            let _ = self.close(); return json!({"state":"idle","suggestion":"","message":"Preview closed.","accepted":self.accepted,"stopped":self.stopped});
        }
        for event in self.worker.poll() {
            let Some(mut request) = self.request.take() else { continue; };
            if event.id != request.id || request.key != self.snapshot.key() || request.generation != self.suggestion.generation || !self.active || !self.owner_released {
                if event.id != request.id { self.request = Some(request); } continue;
            }
            match event.kind.as_str() {
                "ready" => { if self.expected.is_none() && self.eligible() && self.worker.proceed(request.id).is_ok() { request.proceeded = true; self.request = Some(request); } }
                "result" if request.proceeded => {
                    let text = if self.config.completion_length == "word" { features::current_word_only(self.snapshot.prefix(), &event.text) } else { event.text };
                    self.suggestion.offer(request.generation, self.snapshot.clone(), text);
                    if !event.final_ { self.request = Some(request); }
                }
                "error" => { self.error = "Local preview inference failed.".into(); self.suggestion.invalidate(); }
                "done" => {},
                _ => self.request = Some(request),
            }
        }
        let eligible = self.eligible(); let pending = self.expected.is_some();
        if eligible && !pending && self.owner_released && self.error.is_empty() && self.suggestion.text.is_empty() && self.request.is_none()
            && self.attempted.as_ref() != Some(&self.snapshot.key()) && self.clock.now().duration_since(self.changed) >= Duration::from_millis(self.config.debounce_ms.into()) {
            let mut config = self.config.clone(); let (tokens,max_words) = features::length_profile(&config.completion_length); config.max_tokens = config.max_tokens.min(tokens);
            let options = GenerationOptions { instructions: self.config.custom_instructions.chars().take(26000).collect(), max_words, ..Default::default() };
            let start = self.snapshot.caret.saturating_sub(config.context_chars as usize);
            let prefix = self.snapshot.text.slice(start, self.snapshot.caret-start).expect("validated preview range");
            let suffix: String = self.snapshot.suffix().chars().take(config.context_chars as usize).collect();
            match self.worker.submit(&config, prefix, &suffix, &options) {
                Ok(id) => { self.request = Some(Request { id, key: self.snapshot.key(), generation: self.suggestion.generation, proceeded: false }); self.attempted = Some(self.snapshot.key()); }
                Err(_) => self.error = "Local preview inference could not be started.".into(),
            }
        }
        let suggestion = if !pending && eligible && self.suggestion.snapshot.as_ref().is_some_and(|s| s.same_key(&self.snapshot) && s.same_authority(&self.snapshot)) { self.suggestion.text.as_str() } else { "" };
        let (state,message) = if !self.error.is_empty() { ("error",self.error.clone()) }
            else if !suggestion.is_empty() { ("ready","Suggestion ready.".into()) }
            else if self.request.is_some() { let status = self.worker.status(); ("loading",status.get("message").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("Warming up the selected model…").into()) }
            else if eligible && !self.owner_released { ("waiting","Releasing the typing model for this preview…".into()) }
            else if self.accepted > 0 { ("idle",format!("Accepted {} suggestion{}.",self.accepted,if self.accepted == 1 { "" } else { "s" })) }
            else if eligible { ("waiting","Waiting for a typing pause…".into()) }
            else { ("idle","Waiting for you to type…".into()) };
        json!({"state":state,"suggestion":suggestion,"message":message,"accepted":self.accepted})
    }
    fn accept(&mut self, full: bool, visible: Option<&str>) -> String {
        if self.closed || !self.active || !self.owner_released || self.expected.is_some() || !self.eligible() || visible == Some("") { return String::new(); }
        let text = self.suggestion.accept(&self.snapshot,full,self.config.trailing_space,features::include_punctuation(&self.config.punctuation_mode,full),visible).unwrap_or_default();
        if !text.is_empty() { let _ = self.worker.cancel(); self.request = None; self.expected = self.suggestion.snapshot.clone(); }
        text
    }
    pub fn accept_word(&mut self) -> String { self.accept(false, None) }
    pub fn accept_all(&mut self) -> String { self.accept(true, None) }
    /// Native widgets pass the exact fully drawn prefix; clipping never grants
    /// authority to accept a hidden word or a hidden continuation tail.
    pub fn accept_visible(&mut self, full: bool, visible: &str) -> String { self.accept(full, Some(visible)) }
    pub fn dismiss(&mut self) {
        if self.closed { return; } let _ = self.worker.cancel(); self.request = None; self.expected = None; self.suggestion.invalidate(); self.attempted = Some(self.snapshot.key()); self.error.clear();
    }
    /// Retryable, signal-only cleanup; never makes the normal worker resume while
    /// an isolated preview child is still alive.
    pub fn close(&mut self) -> Result<bool> {
        if !self.closed { self.closed = true; self.active = false; self.request = None; self.expected = None; self.suggestion.invalidate(); self.snapshot.text = "".into(); self.snapshot.caret = 0; self.snapshot.selection_length = 0; }
        self.worker.request_stop()?; self.stopped = self.worker.is_stopped(); Ok(self.stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::WorkerEvent;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)] struct State { events: Vec<WorkerEvent>, submits: usize, proceeds: Vec<u64>, cancelled: usize, stopped: bool, options: Option<GenerationOptions> }
    struct FakeWorker(Rc<RefCell<State>>);
    impl Worker for FakeWorker {
        fn prepare(&mut self,_:&Config)->Result<u64>{Ok(0)}
        fn submit(&mut self,_:&Config,_:&str,_:&str,o:&GenerationOptions)->Result<u64>{let mut s=self.0.borrow_mut();s.submits+=1;s.options=Some(o.clone());Ok(s.submits as u64)}
        fn proceed(&mut self,id:u64)->Result<()>{self.0.borrow_mut().proceeds.push(id);Ok(())}
        fn poll(&mut self)->Vec<WorkerEvent>{std::mem::take(&mut self.0.borrow_mut().events)}
        fn cancel(&mut self)->Result<()>{self.0.borrow_mut().cancelled+=1;Ok(())}
        fn release(&mut self)->Result<()>{Ok(())} fn set_idle_timeout(&mut self,_:u32){} fn status(&self)->Value{json!({"message":"loading"})}
        fn request_stop(&mut self)->Result<()>{Ok(())} fn is_stopped(&mut self)->bool{self.0.borrow().stopped}
    }
    struct Time(Rc<RefCell<Instant>>); impl Clock for Time {fn now(&self)->Instant{*self.0.borrow()}}
    fn session()->(PreviewSession,Rc<RefCell<State>>,Rc<RefCell<Instant>>){let s=Rc::new(RefCell::new(State::default()));let t=Rc::new(RefCell::new(Instant::now()));(PreviewSession::with_worker(Config::default(),Box::new(FakeWorker(s.clone())),Box::new(Time(t.clone()))).unwrap(),s,t)}
    fn event(id:u64,kind:&str,text:&str,final_:bool)->WorkerEvent{WorkerEvent{id,kind:kind.into(),text:text.into(),final_,model_loaded:true,message:String::new()}}
    #[test] fn selection_never_borrows_another_models_readiness(){let(mut p,s,t)=session();p.update("Dear team",9,0).unwrap();*t.borrow_mut()+=Duration::from_secs(1);p.poll();assert_eq!(s.borrow().submits,0);p.set_owner_released(true);p.poll();assert_eq!(s.borrow().submits,1);assert!(s.borrow().proceeds.is_empty());s.borrow_mut().events.push(event(1,"ready","",false));p.poll();assert_eq!(s.borrow().proceeds,vec![1]);let o=s.borrow().options.clone().unwrap();assert!(o.context.is_empty()&&o.personalization.is_empty()&&o.utility.is_empty());}
    #[test] fn only_exact_widget_readback_counts_and_dismiss_blocks_retry(){let(mut p,s,t)=session();p.set_owner_released(true);p.update("Dear team",9,0).unwrap();*t.borrow_mut()+=Duration::from_secs(1);p.poll();s.borrow_mut().events.extend([event(1,"ready","",false),event(1,"result",", thank you",true)]);p.poll();let text=p.accept_all();assert_eq!(text,", thank you");assert_eq!(p.poll()["accepted"],0);assert!(p.accept_word().is_empty());p.update("Dear team, thank you",20,0).unwrap();assert_eq!(p.poll()["accepted"],1);p.dismiss();*t.borrow_mut()+=Duration::from_secs(1);p.poll();assert_eq!(s.borrow().submits,1);}
    #[test] fn stale_selection_and_focus_cancel_output_and_cleanup_retries(){let(mut p,s,t)=session();p.set_owner_released(true);p.update("Hello",5,0).unwrap();*t.borrow_mut()+=Duration::from_secs(1);p.poll();p.update("Hello",0,5).unwrap();s.borrow_mut().events.push(event(1,"result"," world",true));assert_eq!(p.poll()["suggestion"],"");p.set_active(false);assert!(p.accept_all().is_empty());assert!(!p.close().unwrap());s.borrow_mut().stopped=true;assert!(p.close().unwrap());}
    #[test]
    fn hidden_preview_and_interior_word_prefix_never_accept_unseen_text() {
        let (mut p,s,t)=session();p.set_owner_released(true);p.update("Hello",5,0).unwrap();*t.borrow_mut()+=Duration::from_secs(1);p.poll();
        s.borrow_mut().events.extend([event(1,"ready","",false),event(1,"result"," world tomorrow",true)]);p.poll();
        assert!(p.accept_visible(true,"").is_empty());assert!(p.accept_visible(false," wo").is_empty());
        assert_eq!(p.accept_visible(true," world")," world ");assert_eq!(p.poll()["accepted"],0);
        p.update("Hello world ",12,0).unwrap();assert_eq!(p.poll()["accepted"],1);
    }
}
