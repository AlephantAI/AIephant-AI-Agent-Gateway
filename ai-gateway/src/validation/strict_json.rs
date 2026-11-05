use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
pub fn parse_json_strict(bytes: &[u8]) -> Result<Value> {
    struct Scanner<'a> {
        bytes: &'a [u8],
        pos: usize,
    }
    impl Scanner<'_> {
        fn whitespace(&mut self) {
            while self.pos < self.bytes.len()
                && matches!(self.bytes[self.pos], b' ' | b'\r' | b'\n' | b'\t')
            {
                self.pos += 1;
            }
        }
        fn string(&mut self) -> Result<String> {
            let start = self.pos;
            self.pos += 1;
            while self.pos < self.bytes.len() {
                match self.bytes[self.pos] {
                    b'\\' => self.pos += 2,
                    b'"' => {
                        self.pos += 1;
                        return Ok(serde_json::from_slice(&self.bytes[start..self.pos])?);
                    }
                    _ => self.pos += 1,
                }
            }
            bail!("unterminated JSON string")
        }
        fn value(&mut self, depth: usize) -> Result<Value> {
            ensure!(depth < 128, "JSON nesting too deep");
            self.whitespace();
            let c = *self.bytes.get(self.pos).context("missing JSON value")?;
            match c {
                b'"' => Ok(Value::String(self.string()?)),
                b'{' | b'[' => {
                    self.pos += 1;
                    self.whitespace();
                    let close = if c == b'{' { b'}' } else { b']' };
                    let mut object = serde_json::Map::new();
                    let mut array = Vec::new();
                    if self.bytes.get(self.pos) == Some(&close) {
                        self.pos += 1;
                        return Ok(if c == b'{' {
                            Value::Object(object)
                        } else {
                            Value::Array(array)
                        });
                    }
                    loop {
                        self.whitespace();
                        if c == b'{' {
                            ensure!(
                                self.bytes.get(self.pos) == Some(&b'"'),
                                "expected object key"
                            );
                            let key = self.string()?;
                            ensure!(!object.contains_key(&key), "duplicate JSON key");
                            self.whitespace();
                            ensure!(self.bytes.get(self.pos) == Some(&b':'), "expected colon");
                            self.pos += 1;
                            object.insert(key, self.value(depth + 1)?);
                        } else {
                            array.push(self.value(depth + 1)?);
                        }
                        self.whitespace();
                        match self.bytes.get(self.pos) {
                            Some(x) if *x == close => {
                                self.pos += 1;
                                break;
                            }
                            Some(b',') => self.pos += 1,
                            _ => bail!("invalid JSON container"),
                        }
                    }
                    Ok(if c == b'{' {
                        Value::Object(object)
                    } else {
                        Value::Array(array)
                    })
                }
                _ => {
                    let start = self.pos;
                    while self.pos < self.bytes.len()
                        && !matches!(
                            self.bytes[self.pos],
                            b',' | b']' | b'}' | b' ' | b'\r' | b'\n' | b'\t'
                        )
                    {
                        self.pos += 1;
                    }
                    ensure!(self.pos > start, "invalid JSON value");
                    Ok(serde_json::from_slice(&self.bytes[start..self.pos])?)
                }
            }
        }
    }
    let mut scanner = Scanner { bytes, pos: 0 };
    let result = scanner.value(0)?;
    scanner.whitespace();
    ensure!(scanner.pos == bytes.len(), "trailing JSON content");
    Ok(result)
}
