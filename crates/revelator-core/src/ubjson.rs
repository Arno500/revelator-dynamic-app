//! Minimal UBJSON (draft 9) reader, the binary format Universal Control uses for its preset
//! documents (`.channel` files are this, zlib-compressed). Found in `dspusbdevice.dll`: the
//! document writer (0x180062e10) has a text mode and a binary mode; the store path
//! (`StoredPreset`, 0x180039524) uses the binary one, so the mic's preset slots most likely
//! hold this format.
//!
//! Layout as seen in real files: `{` object, keys as a length (`i` + byte) and the raw name
//! without an `S` marker, values `i` int8, `d` float32 (big endian), `S` string, `}` end.
//! [`decode`] reads what `presets dump` gets from a slot; [`encode`] writes a document back in
//! the same shape (used to store a slot).

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .b
            .get(self.pos..self.pos + n)
            .ok_or_else(|| format!("truncated at byte {}", self.pos))?;
        self.pos += n;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    /// An integer of the given type marker.
    fn int(&mut self, marker: u8) -> Result<Option<i64>, String> {
        Ok(Some(match marker {
            b'i' => self.byte()? as i8 as i64,
            b'U' => self.byte()? as i64,
            b'I' => i16::from_be_bytes(self.take(2)?.try_into().unwrap()) as i64,
            b'l' => i32::from_be_bytes(self.take(4)?.try_into().unwrap()) as i64,
            b'L' => i64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            _ => return Ok(None),
        }))
    }

    fn length(&mut self) -> Result<usize, String> {
        let m = self.byte()?;
        match self.int(m)? {
            Some(n) if n >= 0 => Ok(n as usize),
            _ => Err(format!(
                "bad length marker {:?} at byte {}",
                m as char,
                self.pos - 1
            )),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let n = self.length()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 32 {
            return Err("nested too deeply".into());
        }
        let m = self.byte()?;
        if let Some(i) = self.int(m)? {
            return Ok(Value::Int(i));
        }
        Ok(match m {
            b'Z' => Value::Null,
            b'T' => Value::Bool(true),
            b'F' => Value::Bool(false),
            b'd' => Value::Float(f32::from_be_bytes(self.take(4)?.try_into().unwrap()) as f64),
            b'D' => Value::Float(f64::from_be_bytes(self.take(8)?.try_into().unwrap())),
            b'C' => Value::Str((self.byte()? as char).to_string()),
            b'S' => Value::Str(self.string()?),
            b'[' => {
                let mut v = Vec::new();
                while self.b.get(self.pos) != Some(&b']') {
                    v.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Value::Array(v)
            }
            b'{' => {
                let mut v = Vec::new();
                while self.b.get(self.pos) != Some(&b'}') {
                    let k = self.string()?;
                    v.push((k, self.value(depth + 1)?));
                }
                self.pos += 1;
                Value::Object(v)
            }
            other => {
                return Err(format!(
                    "unknown type marker 0x{other:02x} at byte {}",
                    self.pos - 1
                ))
            }
        })
    }
}

/// Decode one document; trailing zero padding is ignored.
pub fn decode(bytes: &[u8]) -> Result<Value, String> {
    let mut r = Reader { b: bytes, pos: 0 };
    let v = r.value(0)?;
    if bytes[r.pos..].iter().any(|&b| b != 0) {
        return Err(format!(
            "unexpected data after the document at byte {}",
            r.pos
        ));
    }
    Ok(v)
}

fn put_len(out: &mut Vec<u8>, n: usize) {
    if n <= 127 {
        out.push(b'i');
        out.push(n as u8);
    } else {
        out.push(b'I');
        out.extend_from_slice(&(n as u16).to_be_bytes());
    }
}

fn encode_into(v: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("nested too deeply".into());
    }
    match v {
        Value::Null => out.push(b'Z'),
        Value::Bool(b) => out.push(if *b { b'T' } else { b'F' }),
        Value::Int(i) => {
            if let Ok(x) = i8::try_from(*i) {
                out.push(b'i');
                out.push(x as u8);
            } else if let Ok(x) = i16::try_from(*i) {
                out.push(b'I');
                out.extend_from_slice(&x.to_be_bytes());
            } else if let Ok(x) = i32::try_from(*i) {
                out.push(b'l');
                out.extend_from_slice(&x.to_be_bytes());
            } else {
                out.push(b'L');
                out.extend_from_slice(&i.to_be_bytes());
            }
        }
        Value::Float(x) => {
            if !x.is_finite() {
                return Err("a non-finite number cannot be stored".into());
            }
            out.push(b'd');
            out.extend_from_slice(&(*x as f32).to_be_bytes());
        }
        Value::Str(s) => {
            if s.len() > 0x7FFF {
                return Err("string too long".into());
            }
            out.push(b'S');
            put_len(out, s.len());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(a) => {
            out.push(b'[');
            for x in a {
                encode_into(x, out, depth + 1)?;
            }
            out.push(b']');
        }
        Value::Object(o) => {
            out.push(b'{');
            for (k, x) in o {
                if k.len() > 0x7FFF {
                    return Err("key too long".into());
                }
                put_len(out, k.len());
                out.extend_from_slice(k.as_bytes());
                encode_into(x, out, depth + 1)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// Encode a document the way UC's store path does (floats as 32-bit, keys without an `S`).
pub fn encode(v: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    encode_into(v, &mut out, 0)?;
    Ok(out)
}

impl Value {
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Object(o) => o.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Set `key` in an object (replace or append). No-op on other values.
    pub fn put(&mut self, key: &str, v: Value) {
        if let Value::Object(o) = self {
            match o.iter_mut().find(|(k, _)| k == key) {
                Some(e) => e.1 = v,
                None => o.push((key.to_string(), v)),
            }
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(x) => Some(*x),
            Value::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn write(&self, f: &mut fmt::Formatter<'_>, indent: usize) -> fmt::Result {
        let pad = "  ".repeat(indent);
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Array(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.write(f, indent + 1)?;
                }
                write!(f, "]")
            }
            Value::Object(o) => {
                writeln!(f, "{{")?;
                for (k, v) in o {
                    write!(f, "{pad}  {k:?}: ")?;
                    v.write(f, indent + 1)?;
                    writeln!(f)?;
                }
                write!(f, "{pad}}}")
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, 0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn encode_matches_the_stored_shape() {
        use super::*;
        let doc = Value::Object(vec![
            ("preset_name".into(), Value::Str("Broadcast Basic".into())),
            (
                "gate".into(),
                Value::Object(vec![
                    ("on".into(), Value::Int(1)),
                    ("threshold".into(), Value::Float(-47.5)),
                ]),
            ),
        ]);
        let b = encode(&doc).unwrap();
        assert_eq!(&b[..14], b"{i\x0bpreset_name");
        assert_eq!(decode(&b).unwrap(), doc);
        assert_eq!(encode(&decode(&b).unwrap()).unwrap(), b);
        assert!(encode(&Value::Float(f64::NAN)).is_err());
    }

    use super::*;

    /// Start of a real StudioLive `.channel` file (Fat Channel preset, decompressed).
    const SAMPLE: &[u8] =
        b"{i\x06filter{i\x03hpfdBc\x88\xbf}i\x04gate{i\x02oni\x01i\x09thresholdd\xc1\xf2\x72\x64}\
          i\x04comp{i\x09__classidSi\x26{870D04F7-212E-4F9C-ADBB-39A97216433F}}}";

    #[test]
    fn decodes_a_real_channel_preset() {
        let v = decode(SAMPLE).unwrap();
        let hpf = v.get("filter").unwrap().get("hpf").unwrap();
        assert!(matches!(hpf, Value::Float(x) if (*x - 56.88).abs() < 0.01));
        assert_eq!(v.get("gate").unwrap().get("on"), Some(&Value::Int(1)));
        assert_eq!(
            v.get("comp").unwrap().get("__classid"),
            Some(&Value::Str("{870D04F7-212E-4F9C-ADBB-39A97216433F}".into()))
        );
        assert!(v.to_string().contains("\"hpf\""));
    }

    #[test]
    fn padding_is_fine_garbage_and_truncation_are_errors() {
        let mut padded = SAMPLE.to_vec();
        padded.extend([0; 16]);
        assert!(decode(&padded).is_ok());
        padded.push(7);
        assert!(decode(&padded).is_err());
        assert!(decode(&SAMPLE[..SAMPLE.len() - 5]).is_err());
        assert!(decode(b"?").is_err());
    }
}
