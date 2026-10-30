//! Attach native special integrations before the bridge's first configure.
use super::{MacBridge, terminal::TerminalAdapter, vision::VisionBridge};
use anyhow::Result;
pub fn attach(bridge: &mut MacBridge) -> Result<()> {
    bridge.set_terminal_adapter(Box::new(TerminalAdapter::new()))?;
    bridge.set_appearance_adapter(Box::new(VisionBridge::new()))?;
    Ok(())
}
