//! Internal native shell intents. Values are captured when the native action fires.
use serde_json::Value;
#[derive(Clone, Debug)]
pub(super) enum UiAction {
    Navigate(String), Welcome, Quit,
    Set(String,Value), Rule(String,String,Option<Value>), SelectRule(String), AddApp, AddCurrentApp, AddDomain, RemoveRule(String), FilterRules(String),
    RuleDrop(Vec<(u8,String)>), RemoveExclusion(String),
    ModelSelect(String), ModelDownload(String), ModelImport, ModelCancel, RevealModels, ModelAdvanced(bool),
    Permission(String), RequestScreen, RequestAutomation(String),
    HistoryCounts, DeleteHistory{app:Option<String>,domain:Option<String>}, ClearStatistics,
    Statistics{period:Option<String>,group:Option<String>,metric:Option<String>},
    SaveInstructions(String), RevertInstructions(String), ResetInstructions(String),
    RecordShortcut(String), ShortcutCaptured(String,String),
    UpdateCheck, UpdateDownload, UpdateCancel, UpdateInstall,
    Pause(u32), PauseTomorrow, Resume, ToggleApp{app:String,domain:String}, Docs(Option<String>),
    Url(String), WelcomeStep(i32), PreviewLanguage(String), PreviewAccept(bool), PreviewDismiss,
    PreviewReleased, Resized, Reopened, Deactivated(String), Closed(String),
}
#[derive(Clone)]
pub(super) enum ReadValue { None, Boolean, Text, Integer, Slider, Choice(Vec<Value>) }
#[derive(Clone)]
pub(super) struct Binding { pub action: UiAction, pub read: ReadValue }
#[derive(Default)]
pub(super) struct UiQueue { pub pending: std::collections::VecDeque<UiAction>, pub tracking: usize, pub recording: Option<String>, pub resume_shortcuts: bool }
