//! Mors tablosu: karakter <-> nokta/çizgi dizisi.

/// (karakter, desen) çiftleri. Desen '.' ve '-' içerir.
const TABLE: &[(char, &str)] = &[
    ('A', ".-"),
    ('B', "-..."),
    ('C', "-.-."),
    ('D', "-.."),
    ('E', "."),
    ('F', "..-."),
    ('G', "--."),
    ('H', "...."),
    ('I', ".."),
    ('J', ".---"),
    ('K', "-.-"),
    ('L', ".-.."),
    ('M', "--"),
    ('N', "-."),
    ('O', "---"),
    ('P', ".--."),
    ('Q', "--.-"),
    ('R', ".-."),
    ('S', "..."),
    ('T', "-"),
    ('U', "..-"),
    ('V', "...-"),
    ('W', ".--"),
    ('X', "-..-"),
    ('Y', "-.--"),
    ('Z', "--.."),
    ('0', "-----"),
    ('1', ".----"),
    ('2', "..---"),
    ('3', "...--"),
    ('4', "....-"),
    ('5', "....."),
    ('6', "-...."),
    ('7', "--..."),
    ('8', "---.."),
    ('9', "----."),
    ('.', ".-.-.-"),
    (',', "--..--"),
    ('?', "..--.."),
    ('/', "-..-."),
    ('=', "-...-"),
    ('+', ".-.-."),
    ('-', "-....-"),
    ('(', "-.--."),
    (')', "-.--.-"),
    ('\'', ".----."),
    ('"', ".-..-."),
    (':', "---..."),
    (';', "-.-.-."),
    ('@', ".--.-."),
    ('!', "-.-.--"),
    ('&', ".-..."),
    ('_', "..--.-"),
    ('$', "...-..-"),
];

/// Yalnızca çözmede kullanılan işaretler (prosign). Tablodakilerden önce bakılır.
const PROSIGNS: &[(&str, &str)] = &[
    ("...-.-", "<SK>"),
    ("-.-.-", "<KA>"),
    ("-.--.", "<KN>"),
    ("...-.", "<SN>"),
    ("...---...", "<SOS>"),
    ("........", "<HH>"),
];

/// Bir karakterin desenini döndürür (küçük harf kabul eder).
pub fn encode(c: char) -> Option<&'static str> {
    let c = c.to_ascii_uppercase();
    TABLE.iter().find(|(k, _)| *k == c).map(|(_, p)| *p)
}

/// Bir deseni metne çevirir; bilinmeyen desen için "*".
pub fn decode(pattern: &str) -> &'static str {
    if let Some((_, s)) = PROSIGNS.iter().find(|(p, _)| *p == pattern) {
        return s;
    }
    // TABLE'daki karakterleri &'static str olarak döndürmek için sabit dilim
    const CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.,?/=+-()'\":;@!&_$";
    for (c, p) in TABLE {
        if *p == pattern {
            if let Some(i) = CHARS.find(*c) {
                return &CHARS[i..i + c.len_utf8()];
            }
        }
    }
    "*"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        assert_eq!(decode(encode('Q').unwrap()), "Q");
        assert_eq!(decode("...-.-"), "<SK>");
        assert_eq!(decode("......."), "*");
    }
}
