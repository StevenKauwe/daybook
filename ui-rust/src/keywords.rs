//! Pick a short, verbatim label that survives when the surrounding text shrinks.
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

fn introduces_topic(s: &str) -> bool {
    matches!(
        s,
        "a" | "an" | "the" | "our" | "your" | "my" | "their" | "another" | "some"
    )
}

fn common(s: &str) -> bool {
    if let Some((base, _)) = s.split_once(['\'', '’'])
        && matches!(
            base,
            "i" | "we" | "you" | "they" | "it" | "that" | "there" | "let" | "he" | "she"
        )
    {
        return true;
    }
    static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        "a about above after again all also am an and another any are as ask at
         back be because been before being below both but by can could did do does down each even
         every for from get give going good got great had has have he her here him his how i if in
         into is it its just keep let like little look looks make many me meet might more most much
         my need needs of on one only or other our out over please quite really said same say see
         send she should so some still such take than that the their them then there these they
         think this those time to too up us very was we well were what when where which while who
         why will with would yeah yes yet you your"
            .split_ascii_whitespace()
            .collect()
    });
    COMMON.contains(s)
}

struct Word<'a> {
    start: usize,
    end: usize,
    text: &'a str,
    key: String,
    eligible: bool,
}

pub(super) fn parts(s: &str) -> (&str, &str, &str) {
    let mut words = Vec::new();
    let mut at = 0;
    for token in s.split_whitespace().take(100) {
        let begin = at + s[at..].find(token).unwrap();
        at = begin + token.len();
        let text = token.trim_matches(|c: char| {
            !c.is_alphanumeric() && !matches!(c, '$' | '£' | '€' | '¥' | '%' | '#')
        });
        if text.is_empty() {
            continue;
        }
        let start = begin + token.find(text).unwrap();
        let key = text.to_lowercase();
        let eligible = !common(&key) && text.chars().count() <= 34;
        words.push(Word {
            start,
            end: start + text.len(),
            text,
            key,
            eligible,
        });
    }
    let mut counts = HashMap::new();
    for w in &words {
        *counts.entry(w.key.as_str()).or_insert(0_i32) += 1;
    }
    let mut best = None;
    for (i, w) in words.iter().enumerate().filter(|(_, w)| w.eligible) {
        let mut score = w.text.chars().count().min(12) as i32 + (counts[&*w.key] - 1).min(3) * 5;
        let capital = w.text.starts_with(char::is_uppercase);
        if w.text.chars().any(char::is_numeric) {
            score += 18;
            if w.text.contains(['$', '€', '£', '¥', '%']) {
                score += 25;
            }
        }
        if w.text.chars().any(char::is_uppercase) && !w.text.chars().any(char::is_lowercase) {
            score += 12;
        } else if capital && i > 0 && !s[words[i - 1].end..w.start].contains(['.', '?', '!']) {
            score += 10;
        }
        let mut stop = w.end;
        if let Some(next) = words.get(i + 1)
            && next.eligible
            && s[w.end..next.start]
                .chars()
                .all(|c| c.is_whitespace() && !matches!(c, '\n' | '\r'))
            && s[w.start..next.end].chars().count() <= 28
        {
            let name = capital && next.text.starts_with(char::is_uppercase);
            let topic = i > 0
                && introduces_topic(&words[i - 1].key)
                && w.text.chars().all(char::is_alphabetic)
                && next.text.chars().all(char::is_alphabetic);
            if name || topic {
                stop = next.end;
                score += if name { 20 } else { 12 };
            }
        }
        if best.is_none_or(|(old, _, _)| score > old) {
            best = Some((score, w.start, stop));
        }
    }
    let (lo, hi) = best
        .map(|(_, lo, hi)| (lo, hi))
        .or_else(|| words.first().map(|w| (w.start, w.end)))
        .unwrap_or((0, 0));
    (&s[..lo], &s[lo..hi], &s[hi..])
}

#[cfg(test)]
mod tests {
    fn keyword(s: &str) -> &str {
        super::parts(s).1
    }
    #[test]
    fn compact_labels_are_verbatim_names_amounts_or_repeated_terms() {
        assert_eq!(keyword("We will change the total to $125."), "$125");
        assert_eq!(keyword("The Field Notes cover is ready."), "Field Notes");
        assert_eq!(keyword("proof and proof and proof for printing"), "proof");
        assert_eq!(keyword("Réunion à Montréal vendredi"), "Montréal");
        assert_eq!(keyword("the price dropped to 15% today"), "15%");
    }
    #[test]
    fn compact_labels_keep_the_topic_in_conversational_speech() {
        assert_eq!(
            keyword("We have time for another print proof."),
            "print proof"
        );
        assert_eq!(
            keyword("Keep the original cover in the studio."),
            "original cover"
        );
        assert_eq!(
            keyword("Ask the printer about the green thread."),
            "green thread"
        );
        assert_eq!(
            keyword("Great. I’ll meet you at the press check."),
            "press check"
        );
        assert_eq!(keyword("I’ve got both copies here."), "copies");
    }
}
