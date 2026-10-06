/// Miroir de synco_api/src/services/ai/providerAdapters/thinkTagSplitter.ts — même algorithme,
/// même raison d'être : séparer `<think>...</think>` inline (DeepSeek-R1, QwQ, Mistral Magistral
/// via Ollama) du texte normal, chunk par chunk, sans jamais couper une balise à cheval sur deux
/// chunks ni le milieu d'un caractère UTF-8 multi-octets (texte en français : accents, etc.).
#[derive(Default)]
pub struct ThinkTagSplitter {
    carry: String,
    in_thinking: bool,
}

pub struct SplitResult {
    pub text: String,
    pub thinking: String,
}

impl ThinkTagSplitter {
    pub fn push(&mut self, chunk: &str) -> SplitResult {
        let mut buf = std::mem::take(&mut self.carry);
        buf.push_str(chunk);

        let mut text = String::new();
        let mut thinking = String::new();

        loop {
            let tag = if self.in_thinking { "</think>" } else { "<think>" };
            match buf.find(tag) {
                None => {
                    // `keep` ne matche que des octets ASCII (le tag l'est entièrement), donc
                    // `split_at` tombe toujours sur une frontière de caractère UTF-8 valide.
                    let keep = tail_prefix_len(&buf, tag);
                    let split_at = buf.len() - keep;
                    if self.in_thinking {
                        thinking.push_str(&buf[..split_at]);
                    } else {
                        text.push_str(&buf[..split_at]);
                    }
                    self.carry = buf[split_at..].to_string();
                    return SplitResult { text, thinking };
                }
                Some(at) => {
                    let before = &buf[..at];
                    if self.in_thinking {
                        thinking.push_str(before);
                    } else {
                        text.push_str(before);
                    }
                    buf = buf[at + tag.len()..].to_string();
                    self.in_thinking = !self.in_thinking;
                }
            }
        }
    }
}

/// Plus long suffixe de `s` (en octets) qui est aussi un préfixe strict de `tag` — 0 si aucun.
/// Compare sur `as_bytes()` plutôt que par indexation `&str` : `tag` étant pur ASCII, un suffixe
/// de `s` ne peut matcher que s'il est lui-même ASCII, donc comparer des octets bruts ne risque
/// jamais de couper un caractère multi-octets au milieu (voir la garantie au niveau de `push`).
fn tail_prefix_len(s: &str, tag: &str) -> usize {
    let s_bytes = s.as_bytes();
    let tag_bytes = tag.as_bytes();
    let max = s_bytes.len().min(tag_bytes.len() - 1);
    for len in (1..=max).rev() {
        if tag_bytes.starts_with(&s_bytes[s_bytes.len() - len..]) {
            return len;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_single_complete_block() {
        let mut s = ThinkTagSplitter::default();
        let r = s.push("avant <think>je réfléchis</think> après");
        assert_eq!(r.text, "avant  après");
        assert_eq!(r.thinking, "je réfléchis");
    }

    #[test]
    fn handles_tag_split_across_chunks() {
        let mut s = ThinkTagSplitter::default();
        let r1 = s.push("Bonjour <thi");
        assert_eq!(r1.text, "Bonjour ");
        assert_eq!(r1.thinking, "");

        let r2 = s.push("nk>raisonnement ici</think> le reste");
        assert_eq!(r2.text, " le reste");
        assert_eq!(r2.thinking, "raisonnement ici");
    }

    #[test]
    fn never_panics_on_multibyte_utf8_near_tag_boundary() {
        let mut s = ThinkTagSplitter::default();
        // "café <th" se termine par un ASCII pur après un caractère accentué — vérifie qu'aucun
        // calcul de frontière ne tente de couper "é" (2 octets) au milieu.
        let r1 = s.push("café <th");
        assert_eq!(r1.text, "café ");
        let r2 = s.push("ink>chiffré</think>");
        assert_eq!(r2.thinking, "chiffré");
    }

    #[test]
    fn passthrough_when_no_tag_ever_appears() {
        let mut s = ThinkTagSplitter::default();
        let r = s.push("Rien de spécial ici, juste du texte normal en français.");
        assert_eq!(r.text, "Rien de spécial ici, juste du texte normal en français.");
        assert_eq!(r.thinking, "");
    }
}
