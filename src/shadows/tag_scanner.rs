//! Lightweight JSON scanner for adjacently-tagged enum deserialization.
//!
//! This module provides zero-allocation JSON scanners that extract fields from JSON
//! representations. It's used to deserialize adjacently-tagged enum Delta types and
//! wrapper structures without requiring the `alloc` feature.
//!
//! ## Why This Exists
//!
//! Serde's adjacently-tagged enum deserialization requires `alloc` because JSON field
//! order isn't guaranteed. If `content` appears before `tag`, serde must buffer the
//! content until it knows which variant to deserialize into.
//!
//! This scanner solves the problem by:
//! 1. Scanning the JSON to extract the tag value (if present)
//! 2. Extracting the raw bytes of the content field (if present)
//! 3. Allowing the caller to deserialize the content with type knowledge
//!
//! ## Public APIs
//!
//! - [`TaggedJsonScan`]: Extracts exactly 2 fields (tag + content) for adjacently-tagged enums
//! - [`FieldScanner`]: Extracts N arbitrary fields for wrapper type parsing
//!
//! ## Example: Adjacently-Tagged Enum
//!
//! ```ignore
//! let json = br#"{"config": {"polarity": "pnp"}, "mode": "sio"}"#;
//! let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();
//!
//! let mode = scan.tag_str(); // Some("sio")
//! let content = scan.content_bytes(); // Some(b"{\"polarity\": \"pnp\"}")
//!
//! // Now deserialize content with known type
//! let (config, _): (SioConfig, _) = serde_json_core::from_slice(content.unwrap())?;
//! ```
//!
//! ## Example: Multiple Fields
//!
//! ```ignore
//! let json = br#"{"state": {"desired": null, "delta": {"temp": 25}}, "version": 42}"#;
//! let scanner = FieldScanner::scan(json, &["state", "version"]).unwrap();
//!
//! let state = scanner.field_bytes("state"); // Some(b"{\"desired\": null, \"delta\": {...}}")
//! let version = scanner.field_bytes("version"); // Some(b"42")
//! ```

/// Error type for JSON scanning operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    /// Unexpected end of input
    UnexpectedEof,
    /// Invalid UTF-8 in string
    InvalidUtf8,
    /// Expected a specific byte but found something else
    Expected { expected: u8, found: Option<u8> },
    /// Expected an object at the root level
    ExpectedObject,
    /// Unterminated string literal
    UnterminatedString,
    /// Invalid escape sequence in string
    InvalidEscape,
    /// Nesting depth exceeded (prevents stack overflow on malicious input)
    NestingTooDeep,
}

impl core::fmt::Display for ScanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScanError::UnexpectedEof => write!(f, "unexpected end of input"),
            ScanError::InvalidUtf8 => write!(f, "invalid UTF-8 in string"),
            ScanError::Expected { expected, found } => {
                write!(f, "expected '{}', found ", *expected as char)?;
                match found {
                    Some(b) => write!(f, "'{}'", *b as char),
                    None => write!(f, "EOF"),
                }
            }
            ScanError::ExpectedObject => write!(f, "expected JSON object at root"),
            ScanError::UnterminatedString => write!(f, "unterminated string literal"),
            ScanError::InvalidEscape => write!(f, "invalid escape sequence"),
            ScanError::NestingTooDeep => write!(f, "nesting depth exceeded"),
        }
    }
}

/// Result of scanning a JSON object for tag and content fields.
///
/// Contains borrowed slices into the original JSON input.
#[derive(Debug, Clone)]
pub struct TaggedJsonScan<'a> {
    /// The tag field value, if present and a valid string.
    tag: Option<&'a str>,
    /// The raw JSON bytes of the content field, if present.
    content: Option<&'a [u8]>,
}

impl<'a> TaggedJsonScan<'a> {
    /// Scan a JSON object for tag and content fields.
    ///
    /// # Arguments
    ///
    /// * `json` - The JSON bytes to scan
    /// * `tag_key` - The key name for the tag field (e.g., "mode")
    /// * `content_key` - The key name for the content field (e.g., "config")
    ///
    /// # Returns
    ///
    /// A `TaggedJsonScan` containing the extracted tag value and content bytes,
    /// or an error if the JSON is malformed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let json = br#"{"mode": "sio", "config": {"value": 42}}"#;
    /// let scan = TaggedJsonScan::scan(json, "mode", "config")?;
    /// assert_eq!(scan.tag_str(), Some("sio"));
    /// assert!(scan.content_bytes().is_some());
    /// ```
    pub fn scan(json: &'a [u8], tag_key: &str, content_key: &str) -> Result<Self, ScanError> {
        let mut scanner = Scanner::new(json);
        scanner.scan_for_fields(tag_key, content_key)
    }

    /// Get the tag field value as a string slice.
    ///
    /// Returns `None` if the tag field was not present in the JSON.
    pub fn tag_str(&self) -> Option<&'a str> {
        self.tag
    }

    /// Get the raw JSON bytes of the content field.
    ///
    /// Returns `None` if the content field was not present in the JSON.
    /// The returned bytes are the complete JSON value (object, array, string, etc.)
    /// and can be passed to `serde_json_core::from_slice()` for deserialization.
    pub fn content_bytes(&self) -> Option<&'a [u8]> {
        self.content
    }
}

/// Result of scanning a JSON object for multiple named fields.
///
/// Contains byte ranges into the original JSON input for each requested field.
/// Used for parsing wrapper types like `DeltaResponse` and `AcceptedResponse`
/// where we need to extract several fields without full deserialization.
///
/// # Example
///
/// ```ignore
/// let json = br#"{"state": {"delta": {"temp": 25}}, "version": 42, "timestamp": 1234}"#;
/// let scanner = FieldScanner::scan(json, &["state", "version"]).unwrap();
///
/// let state_bytes = scanner.field_bytes("state"); // Some(b"{\"delta\": ...}")
/// let version_bytes = scanner.field_bytes("version"); // Some(b"42")
/// let timestamp = scanner.field_bytes("timestamp"); // None (not requested)
/// ```
#[derive(Debug, Clone)]
pub struct FieldScanner<'a> {
    /// Byte ranges for each found field: (start, end)
    fields: heapless::LinearMap<&'static str, (usize, usize), 8>,
    /// Reference to the original JSON input
    json: &'a [u8],
}

impl<'a> FieldScanner<'a> {
    /// Scan a JSON object for specific fields.
    ///
    /// # Arguments
    ///
    /// * `json` - The JSON bytes to scan (must be a JSON object)
    /// * `field_names` - Names of fields to extract (max 8)
    ///
    /// # Returns
    ///
    /// A `FieldScanner` containing byte ranges for each found field,
    /// or an error if the JSON is malformed.
    ///
    /// # Panics
    ///
    /// Panics if `field_names` contains more than 8 entries.
    pub fn scan(json: &'a [u8], field_names: &[&'static str]) -> Result<Self, ScanError> {
        assert!(
            field_names.len() <= 8,
            "FieldScanner supports at most 8 fields"
        );

        let mut scanner = Scanner::new(json);
        let fields = scanner.scan_for_multiple_fields(field_names)?;
        Ok(Self { fields, json })
    }

    /// Get the raw JSON bytes for a field.
    ///
    /// Returns `None` if the field was not present in the JSON or
    /// was not in the list of requested field names.
    pub fn field_bytes(&self, name: &str) -> Option<&'a [u8]> {
        for (k, (start, end)) in self.fields.iter() {
            if *k == name {
                return Some(&self.json[*start..*end]);
            }
        }
        None
    }

    /// Get the field value as a string (if it's a JSON string).
    ///
    /// This parses the field as a JSON string and returns the content
    /// without the surrounding quotes. Returns `None` if:
    /// - The field was not present
    /// - The field value is not a string
    /// - The string contains invalid UTF-8
    ///
    /// Note: Escape sequences are validated but not unescaped.
    pub fn field_str(&self, name: &str) -> Option<&'a str> {
        let bytes = self.field_bytes(name)?;
        // Must start and end with quotes
        if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
            return None;
        }
        // Return content without quotes
        core::str::from_utf8(&bytes[1..bytes.len() - 1]).ok()
    }

    /// Check if a field was present in the JSON.
    pub fn has_field(&self, name: &str) -> bool {
        for (k, _) in self.fields.iter() {
            if *k == name {
                return true;
            }
        }
        false
    }
}

/// Internal scanner state.
struct Scanner<'a> {
    input: &'a [u8],
    pos: usize,
}

/// Maximum nesting depth to prevent stack overflow on malicious input.
const MAX_NESTING_DEPTH: usize = 32;

impl<'a> Scanner<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    /// Peek at the current byte without advancing.
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Advance position by one byte.
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Consume the current byte if it matches expected.
    fn expect(&mut self, expected: u8) -> Result<(), ScanError> {
        match self.peek() {
            Some(b) if b == expected => {
                self.advance();
                Ok(())
            }
            found => Err(ScanError::Expected { expected, found }),
        }
    }

    /// Skip whitespace characters.
    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.advance(),
                _ => break,
            }
        }
    }

    /// Parse a JSON string, returning the content (without quotes).
    ///
    /// For simplicity, this returns a slice of the original input.
    /// It validates escape sequences but doesn't unescape them.
    /// This works for tag values which are typically simple identifiers.
    fn parse_string(&mut self) -> Result<&'a str, ScanError> {
        self.expect(b'"')?;
        let start = self.pos;

        loop {
            match self.peek() {
                None => return Err(ScanError::UnterminatedString),
                Some(b'"') => {
                    let end = self.pos;
                    self.advance(); // consume closing quote
                    let bytes = &self.input[start..end];
                    return core::str::from_utf8(bytes).map_err(|_| ScanError::InvalidUtf8);
                }
                Some(b'\\') => {
                    self.advance();
                    // Validate escape sequence
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.advance();
                        }
                        Some(b'u') => {
                            // Unicode escape: \uXXXX
                            self.advance();
                            for _ in 0..4 {
                                match self.peek() {
                                    Some(b) if b.is_ascii_hexdigit() => self.advance(),
                                    _ => return Err(ScanError::InvalidEscape),
                                }
                            }
                        }
                        _ => return Err(ScanError::InvalidEscape),
                    }
                }
                Some(_) => self.advance(),
            }
        }
    }

    /// Skip a JSON string without extracting its value.
    fn skip_string(&mut self) -> Result<(), ScanError> {
        self.expect(b'"')?;

        loop {
            match self.peek() {
                None => return Err(ScanError::UnterminatedString),
                Some(b'"') => {
                    self.advance();
                    return Ok(());
                }
                Some(b'\\') => {
                    self.advance();
                    // Skip the escaped character
                    if self.peek().is_some() {
                        self.advance();
                    }
                }
                Some(_) => self.advance(),
            }
        }
    }

    /// Skip any JSON value, returning the byte range it occupied.
    fn skip_value(&mut self) -> Result<(usize, usize), ScanError> {
        self.skip_value_with_depth(0)
    }

    /// Skip any JSON value with depth tracking.
    fn skip_value_with_depth(&mut self, depth: usize) -> Result<(usize, usize), ScanError> {
        if depth > MAX_NESTING_DEPTH {
            return Err(ScanError::NestingTooDeep);
        }

        self.skip_whitespace();
        let start = self.pos;

        match self.peek() {
            None => Err(ScanError::UnexpectedEof),
            Some(b'"') => {
                self.skip_string()?;
                Ok((start, self.pos))
            }
            Some(b'{') => {
                self.skip_object(depth)?;
                Ok((start, self.pos))
            }
            Some(b'[') => {
                self.skip_array(depth)?;
                Ok((start, self.pos))
            }
            Some(b't') => {
                // true
                self.expect_bytes(b"true")?;
                Ok((start, self.pos))
            }
            Some(b'f') => {
                // false
                self.expect_bytes(b"false")?;
                Ok((start, self.pos))
            }
            Some(b'n') => {
                // null
                self.expect_bytes(b"null")?;
                Ok((start, self.pos))
            }
            Some(b) if b == b'-' || b.is_ascii_digit() => {
                self.skip_number()?;
                Ok((start, self.pos))
            }
            Some(b) => Err(ScanError::Expected {
                expected: b'"',
                found: Some(b),
            }),
        }
    }

    /// Expect and consume a specific byte sequence.
    fn expect_bytes(&mut self, bytes: &[u8]) -> Result<(), ScanError> {
        for &expected in bytes {
            self.expect(expected)?;
        }
        Ok(())
    }

    /// Skip a JSON number.
    fn skip_number(&mut self) -> Result<(), ScanError> {
        // Optional minus
        if self.peek() == Some(b'-') {
            self.advance();
        }

        // Integer part
        match self.peek() {
            Some(b'0') => self.advance(),
            Some(b) if b.is_ascii_digit() => {
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            _ => {
                return Err(ScanError::Expected {
                    expected: b'0',
                    found: self.peek(),
                })
            }
        }

        // Optional fraction
        if self.peek() == Some(b'.') {
            self.advance();
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        // Optional exponent
        if let Some(b'e' | b'E') = self.peek() {
            self.advance();
            if let Some(b'+' | b'-') = self.peek() {
                self.advance();
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Skip a JSON object.
    fn skip_object(&mut self, depth: usize) -> Result<(), ScanError> {
        self.expect(b'{')?;
        self.skip_whitespace();

        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(());
        }

        loop {
            self.skip_whitespace();
            self.skip_string()?; // key
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_value_with_depth(depth + 1)?;
            self.skip_whitespace();

            match self.peek() {
                Some(b',') => self.advance(),
                Some(b'}') => {
                    self.advance();
                    return Ok(());
                }
                found => {
                    return Err(ScanError::Expected {
                        expected: b'}',
                        found,
                    })
                }
            }
        }
    }

    /// Skip a JSON array.
    fn skip_array(&mut self, depth: usize) -> Result<(), ScanError> {
        self.expect(b'[')?;
        self.skip_whitespace();

        if self.peek() == Some(b']') {
            self.advance();
            return Ok(());
        }

        loop {
            self.skip_value_with_depth(depth + 1)?;
            self.skip_whitespace();

            match self.peek() {
                Some(b',') => self.advance(),
                Some(b']') => {
                    self.advance();
                    return Ok(());
                }
                found => {
                    return Err(ScanError::Expected {
                        expected: b']',
                        found,
                    })
                }
            }
        }
    }

    /// Scan a JSON object for specific tag and content fields.
    fn scan_for_fields(
        &mut self,
        tag_key: &str,
        content_key: &str,
    ) -> Result<TaggedJsonScan<'a>, ScanError> {
        let mut tag: Option<&'a str> = None;
        let mut content: Option<&'a [u8]> = None;

        self.skip_whitespace();
        self.expect(b'{')?;

        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(TaggedJsonScan { tag, content });
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();

            if key == tag_key {
                // This is the tag field - extract its string value
                tag = Some(self.parse_string()?);
            } else if key == content_key {
                // This is the content field - capture raw bytes
                let (start, end) = self.skip_value()?;
                content = Some(&self.input[start..end]);
            } else {
                // Unknown field - skip it
                self.skip_value()?;
            }

            self.skip_whitespace();

            match self.peek() {
                Some(b',') => self.advance(),
                Some(b'}') => {
                    self.advance();
                    break;
                }
                found => {
                    return Err(ScanError::Expected {
                        expected: b'}',
                        found,
                    })
                }
            }
        }

        Ok(TaggedJsonScan { tag, content })
    }

    /// Scan a JSON object for multiple named fields.
    fn scan_for_multiple_fields(
        &mut self,
        field_names: &[&'static str],
    ) -> Result<heapless::LinearMap<&'static str, (usize, usize), 8>, ScanError> {
        let mut fields: heapless::LinearMap<&'static str, (usize, usize), 8> =
            heapless::LinearMap::new();

        self.skip_whitespace();
        self.expect(b'{')?;

        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(fields);
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();

            // Check if this is one of the fields we're looking for
            if let Some(&field_name) = field_names.iter().find(|&&name| name == key) {
                let (start, end) = self.skip_value()?;
                // Ignore error from insert - map is full but we continue parsing
                let _ = fields.insert(field_name, (start, end));
            } else {
                // Unknown field - skip it
                self.skip_value()?;
            }

            self.skip_whitespace();

            match self.peek() {
                Some(b',') => self.advance(),
                Some(b'}') => {
                    self.advance();
                    break;
                }
                found => {
                    return Err(ScanError::Expected {
                        expected: b'}',
                        found,
                    })
                }
            }
        }

        Ok(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tag_before_content() {
        let json = br#"{"mode": "sio", "config": {"polarity": "pnp"}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("sio"));
        assert_eq!(scan.content_bytes(), Some(br#"{"polarity": "pnp"}"#.as_slice()));
    }

    #[test]
    fn test_content_before_tag() {
        let json = br#"{"config": {"polarity": "pnp"}, "mode": "sio"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("sio"));
        assert_eq!(scan.content_bytes(), Some(br#"{"polarity": "pnp"}"#.as_slice()));
    }

    #[test]
    fn test_tag_only() {
        let json = br#"{"mode": "inactive"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("inactive"));
        assert_eq!(scan.content_bytes(), None);
    }

    #[test]
    fn test_content_only() {
        let json = br#"{"config": {"polarity": "pnp"}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), None);
        assert_eq!(scan.content_bytes(), Some(br#"{"polarity": "pnp"}"#.as_slice()));
    }

    #[test]
    fn test_neither_present() {
        let json = br#"{"other": "value"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), None);
        assert_eq!(scan.content_bytes(), None);
    }

    #[test]
    fn test_empty_object() {
        let json = br#"{}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), None);
        assert_eq!(scan.content_bytes(), None);
    }

    #[test]
    fn test_nested_content() {
        let json = br#"{"mode": "complex", "config": {"nested": {"deep": [1, 2, 3]}}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("complex"));
        assert_eq!(
            scan.content_bytes(),
            Some(br#"{"nested": {"deep": [1, 2, 3]}}"#.as_slice())
        );
    }

    #[test]
    fn test_content_is_array() {
        let json = br#"{"mode": "list", "config": [1, 2, 3]}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("list"));
        assert_eq!(scan.content_bytes(), Some(br#"[1, 2, 3]"#.as_slice()));
    }

    #[test]
    fn test_content_is_string() {
        let json = br#"{"mode": "simple", "config": "just a string"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("simple"));
        assert_eq!(scan.content_bytes(), Some(br#""just a string""#.as_slice()));
    }

    #[test]
    fn test_content_is_number() {
        let json = br#"{"mode": "numeric", "config": 42.5}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("numeric"));
        assert_eq!(scan.content_bytes(), Some(br#"42.5"#.as_slice()));
    }

    #[test]
    fn test_content_is_null() {
        let json = br#"{"mode": "nullable", "config": null}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("nullable"));
        assert_eq!(scan.content_bytes(), Some(br#"null"#.as_slice()));
    }

    #[test]
    fn test_content_is_bool() {
        let json = br#"{"mode": "flag", "config": true}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("flag"));
        assert_eq!(scan.content_bytes(), Some(br#"true"#.as_slice()));
    }

    #[test]
    fn test_whitespace_variations() {
        let json = br#"  {  "mode"  :  "sio"  ,  "config"  :  {  "x"  :  1  }  }  "#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("sio"));
        assert!(scan.content_bytes().is_some());
    }

    #[test]
    fn test_escaped_string_in_tag() {
        let json = br#"{"mode": "with\"quote", "config": {}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        // Note: we return the raw string including escape sequences
        assert_eq!(scan.tag_str(), Some(r#"with\"quote"#));
    }

    #[test]
    fn test_unicode_escape_in_tag() {
        let json = br#"{"mode": "test\u0041", "config": {}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some(r#"test\u0041"#));
    }

    #[test]
    fn test_extra_fields_ignored() {
        let json = br#"{"before": 1, "mode": "sio", "middle": true, "config": {}, "after": null}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("sio"));
        assert_eq!(scan.content_bytes(), Some(br#"{}"#.as_slice()));
    }

    #[test]
    fn test_error_unterminated_string() {
        let json = br#"{"mode": "unterminated}"#;
        let result = TaggedJsonScan::scan(json, "mode", "config");
        assert!(matches!(result, Err(ScanError::UnterminatedString)));
    }

    #[test]
    fn test_error_expected_object() {
        let json = br#"["not", "an", "object"]"#;
        let result = TaggedJsonScan::scan(json, "mode", "config");
        assert!(matches!(result, Err(ScanError::Expected { expected: b'{', .. })));
    }

    #[test]
    fn test_error_unexpected_eof() {
        let json = br#"{"mode": "#;
        let result = TaggedJsonScan::scan(json, "mode", "config");
        assert!(result.is_err());
    }

    #[test]
    fn test_negative_number_in_content() {
        let json = br#"{"mode": "num", "config": -123.45e-6}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert_eq!(scan.tag_str(), Some("num"));
        assert_eq!(scan.content_bytes(), Some(br#"-123.45e-6"#.as_slice()));
    }

    // FieldScanner tests

    #[test]
    fn test_field_scanner_basic() {
        let json = br#"{"state": {"delta": {"temp": 25}}, "version": 42}"#;
        let scanner = FieldScanner::scan(json, &["state", "version"]).unwrap();

        assert_eq!(
            scanner.field_bytes("state"),
            Some(br#"{"delta": {"temp": 25}}"#.as_slice())
        );
        assert_eq!(scanner.field_bytes("version"), Some(b"42".as_slice()));
    }

    #[test]
    fn test_field_scanner_missing_field() {
        let json = br#"{"state": {}, "version": 42}"#;
        let scanner = FieldScanner::scan(json, &["state", "timestamp"]).unwrap();

        assert!(scanner.has_field("state"));
        assert!(!scanner.has_field("timestamp"));
        assert_eq!(scanner.field_bytes("timestamp"), None);
    }

    #[test]
    fn test_field_scanner_empty_object() {
        let json = br#"{}"#;
        let scanner = FieldScanner::scan(json, &["state", "version"]).unwrap();

        assert!(!scanner.has_field("state"));
        assert!(!scanner.has_field("version"));
    }

    #[test]
    fn test_field_scanner_string_field() {
        let json = br#"{"name": "test", "value": 123}"#;
        let scanner = FieldScanner::scan(json, &["name", "value"]).unwrap();

        assert_eq!(scanner.field_str("name"), Some("test"));
        assert_eq!(scanner.field_str("value"), None); // not a string
        assert_eq!(scanner.field_bytes("value"), Some(b"123".as_slice()));
    }

    #[test]
    fn test_field_scanner_extra_fields_ignored() {
        let json = br#"{"a": 1, "b": 2, "c": 3, "d": 4}"#;
        let scanner = FieldScanner::scan(json, &["b", "d"]).unwrap();

        assert_eq!(scanner.field_bytes("b"), Some(b"2".as_slice()));
        assert_eq!(scanner.field_bytes("d"), Some(b"4".as_slice()));
        assert!(!scanner.has_field("a"));
        assert!(!scanner.has_field("c"));
    }

    #[test]
    fn test_field_scanner_nested_object() {
        let json = br#"{"outer": {"inner": {"deep": [1, 2, 3]}}}"#;
        let scanner = FieldScanner::scan(json, &["outer"]).unwrap();

        assert_eq!(
            scanner.field_bytes("outer"),
            Some(br#"{"inner": {"deep": [1, 2, 3]}}"#.as_slice())
        );
    }

    #[test]
    fn test_field_scanner_all_json_types() {
        let json = br#"{"str": "hello", "num": 42, "bool": true, "null": null, "arr": [1], "obj": {}}"#;
        let scanner =
            FieldScanner::scan(json, &["str", "num", "bool", "null", "arr", "obj"]).unwrap();

        assert_eq!(scanner.field_bytes("str"), Some(br#""hello""#.as_slice()));
        assert_eq!(scanner.field_bytes("num"), Some(b"42".as_slice()));
        assert_eq!(scanner.field_bytes("bool"), Some(b"true".as_slice()));
        assert_eq!(scanner.field_bytes("null"), Some(b"null".as_slice()));
        assert_eq!(scanner.field_bytes("arr"), Some(b"[1]".as_slice()));
        assert_eq!(scanner.field_bytes("obj"), Some(b"{}".as_slice()));
    }

    #[test]
    fn test_field_scanner_whitespace() {
        let json = br#"  {  "a"  :  1  ,  "b"  :  2  }  "#;
        let scanner = FieldScanner::scan(json, &["a", "b"]).unwrap();

        assert_eq!(scanner.field_bytes("a"), Some(b"1".as_slice()));
        assert_eq!(scanner.field_bytes("b"), Some(b"2".as_slice()));
    }
}
