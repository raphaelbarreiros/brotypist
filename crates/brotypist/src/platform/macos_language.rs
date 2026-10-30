//! Caller-owned, local NaturalLanguage recognition; never changes user settings.
use anyhow::{Result, anyhow};
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::NSString;
use std::{collections::BTreeMap, marker::PhantomData, rc::Rc};
#[link(name = "NaturalLanguage", kind = "framework")]
unsafe extern "C" {}

pub struct LanguageRecognizer {
    native: Retained<AnyObject>,
    _owner: PhantomData<Rc<()>>,
}
impl LanguageRecognizer {
    pub fn new() -> Result<Self> {
        let class = AnyClass::get(c"NLLanguageRecognizer")
            .ok_or_else(|| anyhow!("language-unavailable"))?;
        let raw: *mut AnyObject = unsafe { msg_send![class, new] };
        let native =
            unsafe { Retained::from_raw(raw) }.ok_or_else(|| anyhow!("language-unavailable"))?;
        Ok(Self {
            native,
            _owner: PhantomData,
        })
    }
    pub fn identify(&mut self, text: &str) -> Result<BTreeMap<String, f64>> {
        struct Reset<'a>(&'a AnyObject);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                unsafe {
                    let _: () = msg_send![self.0, reset];
                }
            }
        }
        unsafe {
            let _: () = msg_send![&*self.native, reset];
        }
        let _reset = Reset(&self.native);
        if text.trim().is_empty() {
            return Ok(BTreeMap::new());
        }
        let value = NSString::from_str(text);
        unsafe {
            let _: () = msg_send![&*self.native, processString: &*value];
            let dictionary: *mut AnyObject =
                msg_send![&*self.native, languageHypothesesWithMaximum: 5usize];
            if dictionary.is_null() {
                return Ok(BTreeMap::new());
            }
            let keys: *mut AnyObject = msg_send![dictionary, allKeys];
            let count: usize = msg_send![keys, count];
            if count > 5 {
                return Err(anyhow!("language-result-unbounded"));
            }
            let mut result = BTreeMap::new();
            for index in 0..count {
                let key: *mut NSString = msg_send![keys, objectAtIndex: index];
                let score: *mut AnyObject = msg_send![dictionary, objectForKey: key];
                if key.is_null() || score.is_null() {
                    return Err(anyhow!("language-result-unavailable"));
                }
                let value: f64 = msg_send![score, doubleValue];
                result.insert((&*key).to_string(), value);
            }
            Ok(result)
        }
    }
}
pub fn confident(hypotheses: &BTreeMap<String, f64>) -> Option<String> {
    let mut ranked: Vec<_> = hypotheses.iter().filter(|(_, v)| v.is_finite()).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(a.1));
    let (tag, score) = ranked.first()?;
    if **score < 0.90 || **score - ranked.get(1).map_or(0.0, |(_, v)| **v) < 0.80 {
        return None;
    }
    let mut pieces = tag.split('-');
    let first = pieces.next()?;
    if !(2..=8).contains(&first.len())
        || !first.bytes().all(|c| c.is_ascii_alphabetic())
        || !pieces
            .all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|c| c.is_ascii_alphanumeric()))
    {
        return None;
    }
    Some((*tag).clone())
}
pub fn draft_language_hint(prefix: &str) -> Result<Option<String>> {
    let start = prefix.char_indices().rev().nth(1199).map_or(0, |(i, _)| i);
    let draft = &prefix[start..];
    let tail = draft
        .rsplit(['.', '!', '?', '。', '！', '？', '\n'])
        .next()
        .unwrap_or(draft);
    let text = if tail.chars().filter(|c| c.is_alphabetic()).count() >= 6 {
        tail
    } else {
        draft
    };
    if text.chars().filter(|c| c.is_alphabetic()).count() < 6 {
        return Ok(None);
    }
    Ok(confident(&LanguageRecognizer::new()?.identify(text)?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confidence_is_advisory_and_finite() {
        for scores in [
            BTreeMap::from([("en".into(), 0.89)]),
            BTreeMap::from([("en".into(), 0.95), ("pt".into(), 0.16)]),
            BTreeMap::from([("en".into(), f64::NAN)]),
            BTreeMap::from([("en_bad".into(), 1.0)]),
        ] {
            assert_eq!(confident(&scores), None)
        }
        assert_eq!(
            confident(&BTreeMap::from([
                ("pt-BR".into(), 0.99),
                ("en".into(), 0.01)
            ])),
            Some("pt-BR".into())
        );
    }
}
