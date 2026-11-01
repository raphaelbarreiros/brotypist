//! NSSpellChecker session: installed dictionaries only, no shared settings mutation.
use super::language::LanguageRecognizer;
use crate::{
    core::casefold,
    spelling::{SpellBackend, SpellWorkScope},
};
use anyhow::{Result, anyhow};
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSRange, NSString};
use std::{
    collections::BTreeMap,
    thread::{self, ThreadId},
};

pub struct MacSpellBackend {
    checker: Retained<AnyObject>,
    tag: Option<isize>,
    recognizer: Option<LanguageRecognizer>,
    owner: Option<ThreadId>,
    closed: bool,
}
// The shared AppKit checker is acquired on main. Once moved to the spelling
// worker, all mutable native work (including cleanup) is thread-confined by enter.
// No caller may invoke native work on an arbitrary thread after first entry.
unsafe impl Send for MacSpellBackend {}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "System")]
unsafe extern "C" {
    fn pthread_main_np() -> i32;
}
impl MacSpellBackend {
    pub fn new() -> Result<Self> {
        let class =
            AnyClass::get(c"NSSpellChecker").ok_or_else(|| anyhow!("spelling-unavailable"))?;
        unsafe {
            let exists: bool = msg_send![class, sharedSpellCheckerExists];
            if pthread_main_np() == 0 && !exists {
                return Err(anyhow!("spelling-main-thread-acquisition-required"));
            }
            let checker: *mut AnyObject = msg_send![class, sharedSpellChecker];
            let checker =
                Retained::retain(checker).ok_or_else(|| anyhow!("spelling-unavailable"))?;
            Ok(Self {
                checker,
                tag: None,
                recognizer: None,
                owner: None,
                closed: false,
            })
        }
    }
    fn enter(&mut self) -> Result<()> {
        if self.closed {
            return Err(anyhow!("spelling-closed"));
        }
        let current = thread::current().id();
        if self.owner.is_some_and(|owner| owner != current) {
            return Err(anyhow!("spelling-owner-thread-required"));
        }
        self.owner = Some(current);
        if self.tag.is_none() {
            let class =
                AnyClass::get(c"NSSpellChecker").ok_or_else(|| anyhow!("spelling-unavailable"))?;
            self.tag = Some(unsafe { msg_send![class, uniqueSpellDocumentTag] });
        }
        Ok(())
    }
    fn language(&mut self, language: &str) -> Result<Retained<NSString>> {
        self.enter()?;
        let (available, _) = self.languages()?;
        if !available.iter().any(|v| v == language) {
            return Err(anyhow!("spelling-language-unavailable"));
        }
        Ok(NSString::from_str(language))
    }
}
struct Pool(*mut std::ffi::c_void);
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_autoreleasePoolPush() -> *mut std::ffi::c_void;
    fn objc_autoreleasePoolPop(pool: *mut std::ffi::c_void);
}
impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}
unsafe fn strings(array: *mut AnyObject, limit: usize) -> Result<Vec<String>> {
    if array.is_null() {
        return Ok(Vec::new());
    }
    let count: usize = unsafe { msg_send![array, count] };
    if count > 4096 {
        return Err(anyhow!("spelling-result-unbounded"));
    }
    let mut result = Vec::with_capacity(count.min(limit));
    for i in 0..count.min(limit) {
        let item: *mut NSString = unsafe { msg_send![array,objectAtIndex:i] };
        if item.is_null() {
            return Err(anyhow!("spelling-result-unavailable"));
        }
        result.push(unsafe { &*item }.to_string());
    }
    Ok(result)
}
impl SpellBackend for MacSpellBackend {
    fn work_scope(&mut self) -> Result<Box<dyn SpellWorkScope>> {
        self.enter()?;
        Ok(Box::new(Pool(unsafe { objc_autoreleasePoolPush() })))
    }
    fn languages(&mut self) -> Result<(Vec<String>, Vec<String>)> {
        self.enter()?;
        unsafe {
            let array: *mut AnyObject = msg_send![&*self.checker, availableLanguages];
            let available = strings(array, 4096)?;
            let locale =
                AnyClass::get(c"NSLocale").ok_or_else(|| anyhow!("spelling-unavailable"))?;
            let locales: *mut AnyObject = msg_send![locale, preferredLanguages];
            let mut preferred = Vec::new();
            for item in strings(locales, 4096)? {
                let tag = item.replace('-', "_");
                let base = tag.split('_').next().unwrap_or("");
                if let Some(found) = available.iter().find(|v| **v == tag).or_else(|| {
                    available
                        .iter()
                        .find(|v| v.split('_').next().unwrap_or("").eq_ignore_ascii_case(base))
                }) {
                    if !preferred.contains(found) {
                        preferred.push(found.clone())
                    }
                }
            }
            let current: *mut NSString = msg_send![&*self.checker, language];
            if !current.is_null() {
                let current = (&*current).to_string();
                if available.contains(&current) && !preferred.contains(&current) {
                    preferred.push(current)
                }
            }
            Ok((available, preferred))
        }
    }
    fn identify(&mut self, text: &str) -> Result<BTreeMap<String, f64>> {
        self.enter()?;
        if text.trim().is_empty() {
            return Ok(BTreeMap::new());
        }
        if self.recognizer.is_none() {
            self.recognizer = Some(LanguageRecognizer::new()?)
        }
        self.recognizer.as_mut().unwrap().identify(text)
    }
    fn misspelled(&mut self, word: &str, language: &str) -> Result<Vec<(usize, usize)>> {
        let language = self.language(language)?;
        let text = NSString::from_str(word);
        let units = word.encode_utf16().count();
        let mut start = 0;
        let mut ranges = Vec::new();
        while start < units && ranges.len() < 8 {
            let range: NSRange = unsafe {
                msg_send![&*self.checker,checkSpellingOfString:&*text,startingAt:start as isize,language:&*language,wrap:false,inSpellDocumentWithTag:self.tag.unwrap(),wordCount:std::ptr::null_mut::<isize>()]
            };
            if range.length == 0 || range.location < start || range.location >= units {
                break;
            }
            ranges.push((range.location, range.length.min(units - range.location)));
            let Some(next) = range.location.checked_add(range.length) else {
                break;
            };
            start = next;
        }
        Ok(ranges)
    }
    fn has_completions(&mut self, partial: &str, language: &str) -> Result<bool> {
        let language = self.language(language)?;
        let text = NSString::from_str(partial);
        let values: *mut AnyObject = unsafe {
            msg_send![&*self.checker,completionsForPartialWordRange:NSRange::new(0,partial.encode_utf16().count()),inString:&*text,language:&*language,inSpellDocumentWithTag:self.tag.unwrap()]
        };
        let folded = casefold(partial);
        Ok(unsafe { strings(values, 4096) }?
            .iter()
            .any(|v| casefold(v) != folded))
    }
    fn suggestions(
        &mut self,
        text: &str,
        start: usize,
        length: usize,
        language: &str,
    ) -> Result<(String, Vec<String>)> {
        if crate::spelling::utf16_slice(
            text,
            start,
            start
                .checked_add(length)
                .ok_or_else(|| anyhow!("spelling-range-invalid"))?,
        )
        .is_none()
        {
            return Err(anyhow!("spelling-range-invalid"));
        }
        let language = self.language(language)?;
        let text = NSString::from_str(text);
        let range = NSRange::new(start, length);
        unsafe {
            let correction: *mut NSString = msg_send![&*self.checker,correctionForWordRange:range,inString:&*text,language:&*language,inSpellDocumentWithTag:self.tag.unwrap()];
            let guesses: *mut AnyObject = msg_send![&*self.checker,guessesForWordRange:range,inString:&*text,language:&*language,inSpellDocumentWithTag:self.tag.unwrap()];
            Ok((
                if correction.is_null() {
                    String::new()
                } else {
                    (&*correction).to_string()
                },
                strings(guesses, 5)?,
            ))
        }
    }
    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        if self.owner.is_some_and(|id| id != thread::current().id()) {
            return Err(anyhow!("spelling-owner-thread-required"));
        }
        self.recognizer = None;
        if let Some(tag) = self.tag.take() {
            unsafe {
                let _: () = msg_send![&*self.checker,closeSpellDocumentWithTag:tag];
            }
        }
        self.closed = true;
        Ok(())
    }
}
impl Drop for MacSpellBackend {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
pub fn spelling_backend() -> Option<Box<dyn SpellBackend>> {
    MacSpellBackend::new()
        .ok()
        .map(|backend| Box::new(backend) as Box<dyn SpellBackend>)
}
#[cfg(test)]
mod tests {
    #[test]
    fn cocoa_ranges_count_utf16() {
        assert_eq!("😀e\u{301}".encode_utf16().count(), 4);
        assert!(crate::spelling::utf16_slice("😀word", 1, 3).is_none());
    }
}
