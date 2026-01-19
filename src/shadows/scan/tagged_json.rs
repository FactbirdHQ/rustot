//! TaggedJsonScan: A lightweight `no_std`, `no_alloc` JSON scanner for adjacently-tagged enums.
//!
//! This utility scans JSON to find byte ranges of tag and content fields without fully
//! parsing the JSON. This enables deferred parsing where the content is only parsed
//! after the variant is determined.

use core::ops::Range;

use crate::shadows::error::ScanError;

/// Result of scanning a JSON object for tag/content fields.
///
/// This utility scans JSON to find byte ranges without fully parsing,
/// enabling deferred parsing of the content field after the tag is known.
///
/// # Example
///
/// ```ignore
/// let json = br#"{"config": {"polarity": "pnp"}, "mode": "sio"}"#;
/// let scan = TaggedJsonScan::scan(json, "mode", "config")?;
///
/// // Tag can be extracted first, regardless of JSON field order
/// let mode: Option<&str> = scan.tag_str()?;
///
/// // Content parsed later, after variant is determined
/// if let Some(bytes) = scan.content_bytes() {
///     // Parse bytes as the appropriate config type
/// }
/// ```
pub struct TaggedJsonScan<'a> {
    json: &'a [u8],
    tag_range: Option<Range<usize>>,
    content_range: Option<Range<usize>>,
}

impl<'a> TaggedJsonScan<'a> {
    /// Scan a JSON object for the specified tag and content fields.
    ///
    /// This performs a lightweight scan that:
    /// 1. Finds the byte range of the tag field's value
    /// 2. Finds the byte range of the content field's value
    /// 3. Does NOT fully parse either field
    ///
    /// Field order does not matter - both fields are located in a single pass.
    pub fn scan(json: &'a [u8], tag_field: &str, content_field: &str) -> Result<Self, ScanError> {
        let mut scanner = JsonScanner::new(json);
        let mut tag_range = None;
        let mut content_range = None;

        // Expect opening brace
        scanner.skip_whitespace();
        scanner.expect_byte(b'{')?;

        let mut first_field = true;

        loop {
            scanner.skip_whitespace();

            // Check for end of object
            if scanner.peek() == Some(b'}') {
                scanner.advance();
                break;
            }

            // Handle comma between fields (after first field)
            if !first_field && scanner.peek() == Some(b',') {
                scanner.advance();
                scanner.skip_whitespace();
            }
            first_field = false;

            // Check for end after comma (shouldn't happen in valid JSON, but be safe)
            if scanner.peek() == Some(b'}') {
                scanner.advance();
                break;
            }

            // Parse field name
            let field_name = scanner.parse_string()?;

            scanner.skip_whitespace();
            scanner.expect_byte(b':')?;
            scanner.skip_whitespace();

            // Record byte range of value, then skip over it
            let value_start = scanner.pos;
            scanner.skip_value()?;
            let value_end = scanner.pos;

            if field_name == tag_field {
                tag_range = Some(value_start..value_end);
            } else if field_name == content_field {
                content_range = Some(value_start..value_end);
            }
        }

        Ok(Self {
            json,
            tag_range,
            content_range,
        })
    }

    /// Get the tag field value as a string slice (without JSON quotes).
    ///
    /// Returns `Ok(None)` if the tag field was not present in the JSON.
    /// Returns `Err` if the tag value is not a valid JSON string.
    ///
    /// Note: The returned string contains raw escape sequences (e.g., `\"`)
    /// as they appear in the JSON. No unescaping is performed.
    pub fn tag_str(&self) -> Result<Option<&'a str>, ScanError> {
        match &self.tag_range {
            Some(range) => {
                let bytes = &self.json[range.clone()];

                // Tag should be a JSON string, verify and extract content
                if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
                    return Err(ScanError::UnexpectedToken);
                }

                // Extract string content (between quotes)
                let content = &bytes[1..bytes.len() - 1];
                let s = core::str::from_utf8(content).map_err(|_| ScanError::InvalidJson)?;
                Ok(Some(s))
            }
            None => Ok(None),
        }
    }

    /// Get the raw bytes of the content field for deferred parsing.
    ///
    /// Returns `None` if the content field was not present in the JSON.
    pub fn content_bytes(&self) -> Option<&'a [u8]> {
        self.content_range.clone().map(|r| &self.json[r])
    }

    /// Check if the tag field was present in the JSON.
    pub fn has_tag(&self) -> bool {
        self.tag_range.is_some()
    }

    /// Check if the content field was present in the JSON.
    pub fn has_content(&self) -> bool {
        self.content_range.is_some()
    }
}

/// Internal JSON scanner - minimal implementation for object field scanning.
struct JsonScanner<'a> {
    json: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn new(json: &'a [u8]) -> Self {
        Self { json, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.json.get(self.pos).copied()
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), ScanError> {
        match self.peek() {
            Some(b) if b == expected => {
                self.advance();
                Ok(())
            }
            Some(_) => Err(ScanError::UnexpectedToken),
            None => Err(ScanError::InvalidJson),
        }
    }

    /// Parse a JSON string and return its content (without quotes).
    ///
    /// The returned string contains raw escape sequences as they appear in JSON.
    fn parse_string(&mut self) -> Result<&'a str, ScanError> {
        self.expect_byte(b'"')?;
        let start = self.pos;

        while let Some(b) = self.peek() {
            if b == b'"' {
                let end = self.pos;
                self.advance(); // consume closing quote
                let bytes = &self.json[start..end];
                return core::str::from_utf8(bytes).map_err(|_| ScanError::InvalidJson);
            } else if b == b'\\' {
                self.advance(); // skip backslash
                if self.peek().is_none() {
                    return Err(ScanError::InvalidJson);
                }
                self.advance(); // skip escaped char
            } else {
                self.advance();
            }
        }

        Err(ScanError::InvalidJson)
    }

    /// Skip over a JSON value (string, number, object, array, bool, null).
    fn skip_value(&mut self) -> Result<(), ScanError> {
        self.skip_whitespace();

        match self.peek() {
            Some(b'"') => {
                // String
                self.advance();
                self.skip_string_contents()
            }
            Some(b'{') => {
                // Object - track brace depth, handle strings
                let mut depth = 1;
                self.advance();
                while depth > 0 {
                    match self.peek() {
                        Some(b'{') => {
                            depth += 1;
                            self.advance();
                        }
                        Some(b'}') => {
                            depth -= 1;
                            self.advance();
                        }
                        Some(b'"') => {
                            self.advance();
                            self.skip_string_contents()?;
                        }
                        Some(_) => self.advance(),
                        None => return Err(ScanError::InvalidJson),
                    }
                }
                Ok(())
            }
            Some(b'[') => {
                // Array - track bracket depth, handle nested objects and strings
                let mut bracket_depth = 1;
                self.advance();
                while bracket_depth > 0 {
                    match self.peek() {
                        Some(b'[') => {
                            bracket_depth += 1;
                            self.advance();
                        }
                        Some(b']') => {
                            bracket_depth -= 1;
                            self.advance();
                        }
                        Some(b'{') => {
                            // Skip entire nested object
                            self.advance();
                            self.skip_object_contents()?;
                        }
                        Some(b'"') => {
                            self.advance();
                            self.skip_string_contents()?;
                        }
                        Some(_) => self.advance(),
                        None => return Err(ScanError::InvalidJson),
                    }
                }
                Ok(())
            }
            Some(b) if b == b'-' || b.is_ascii_digit() => {
                // Number
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit()
                        || b == b'.'
                        || b == b'-'
                        || b == b'+'
                        || b == b'e'
                        || b == b'E'
                    {
                        self.advance();
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Some(b't') => {
                // true
                for expected in b"true" {
                    self.expect_byte(*expected)?;
                }
                Ok(())
            }
            Some(b'f') => {
                // false
                for expected in b"false" {
                    self.expect_byte(*expected)?;
                }
                Ok(())
            }
            Some(b'n') => {
                // null
                for expected in b"null" {
                    self.expect_byte(*expected)?;
                }
                Ok(())
            }
            Some(_) => Err(ScanError::UnexpectedToken),
            None => Err(ScanError::InvalidJson),
        }
    }

    /// Skip object contents (assumes opening brace already consumed).
    fn skip_object_contents(&mut self) -> Result<(), ScanError> {
        let mut depth = 1;
        while depth > 0 {
            match self.peek() {
                Some(b'{') => {
                    depth += 1;
                    self.advance();
                }
                Some(b'}') => {
                    depth -= 1;
                    self.advance();
                }
                Some(b'"') => {
                    self.advance();
                    self.skip_string_contents()?;
                }
                Some(_) => self.advance(),
                None => return Err(ScanError::InvalidJson),
            }
        }
        Ok(())
    }

    /// Skip string contents (assumes opening quote already consumed).
    fn skip_string_contents(&mut self) -> Result<(), ScanError> {
        while let Some(b) = self.peek() {
            if b == b'"' {
                self.advance();
                return Ok(());
            } else if b == b'\\' {
                self.advance(); // skip backslash
                if self.peek().is_none() {
                    return Err(ScanError::InvalidJson);
                }
                self.advance(); // skip escaped char
            } else {
                self.advance();
            }
        }
        Err(ScanError::InvalidJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // 6.1 Nested Object Depth Tracking
    // =========================================================================

    #[test]
    fn test_nested_object_in_config() {
        // Deep nesting - must not get confused by intermediate braces
        let json = br#"{"mode": "sio", "config": {"inner": {"deep": 42}, "other": true}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, "sio");

        // Config should be the entire nested object
        let config = scan.content_bytes().unwrap();
        assert_eq!(config, br#"{"inner": {"deep": 42}, "other": true}"#);
    }

    #[test]
    fn test_braces_inside_string_not_counted() {
        // String contains braces - must not break depth tracking
        let json = br#"{"mode": "test", "config": {"msg": "hello { world } }", "val": 1}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        // Should get entire config, not truncate at the "}" inside the string
        let config = scan.content_bytes().unwrap();
        assert_eq!(config, br#"{"msg": "hello { world } }", "val": 1}"#);
    }

    #[test]
    fn test_array_with_nested_objects() {
        // Array containing objects - bracket and brace depth both matter
        let json = br#"{"mode": "multi", "config": [{"a": 1}, {"b": [2, 3]}]}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, br#"[{"a": 1}, {"b": [2, 3]}]"#);
    }

    // =========================================================================
    // 6.2 String Escape Sequences
    // =========================================================================

    #[test]
    fn test_escaped_quote_in_string_value() {
        // Escaped quote inside string - must not end string early
        let json = br#"{"mode": "say \"hello\"", "config": {"x": 1}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, r#"say \"hello\""#);
    }

    #[test]
    fn test_escaped_backslash_before_quote() {
        // "\\" followed by quote - the quote is NOT escaped
        let json = br#"{"mode": "path\\", "config": {}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, r#"path\\"#);
    }

    #[test]
    fn test_field_name_with_escape() {
        // Field name contains escaped chars - still finds correct field
        let json = br#"{"mo\"de": "wrong", "mode": "right", "config": {}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, "right");
    }

    // =========================================================================
    // 6.3 Field Order and Missing Fields
    // =========================================================================

    #[test]
    fn test_config_before_mode() {
        // Reversed order from typical - must still work
        let json = br#"{"config": {"polarity": "pnp"}, "mode": "sio"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, "sio");
        assert!(scan.content_bytes().is_some());
    }

    #[test]
    fn test_mode_absent_config_present() {
        // Partial update - mode missing means "keep current variant"
        let json = br#"{"config": {"polarity": "npn"}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(!scan.has_tag());
        assert!(scan.has_content());
    }

    #[test]
    fn test_extra_fields_ignored() {
        // Unknown fields between mode and config - must skip correctly
        let json =
            br#"{"mode": "sio", "timestamp": 12345, "metadata": {"foo": "bar"}, "config": {}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(scan.has_tag());
        assert!(scan.has_content());
    }

    // =========================================================================
    // Additional tests from archive design doc
    // =========================================================================

    #[test]
    fn test_mode_before_config() {
        let json = br#"{"mode": "sio", "config": {"polarity": "pnp"}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(scan.has_tag());
        assert!(scan.has_content());

        let mode = scan.tag_str().unwrap().unwrap();
        assert_eq!(mode, "sio");
    }

    #[test]
    fn test_config_only() {
        let json = br#"{"config": {"polarity": "pnp"}}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(!scan.has_tag());
        assert!(scan.has_content());
    }

    #[test]
    fn test_mode_only() {
        let json = br#"{"mode": "inactive"}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(scan.has_tag());
        assert!(!scan.has_content());
    }

    #[test]
    fn test_empty_object() {
        let json = br#"{}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        assert!(!scan.has_tag());
        assert!(!scan.has_content());
    }

    #[test]
    fn test_numeric_values() {
        let json = br#"{"mode": "test", "config": 42}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"42");
    }

    #[test]
    fn test_boolean_values() {
        let json = br#"{"mode": "test", "config": true}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"true");
    }

    #[test]
    fn test_null_value() {
        let json = br#"{"mode": "test", "config": null}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"null");
    }

    #[test]
    fn test_array_value() {
        let json = br#"{"mode": "test", "config": [1, 2, 3]}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"[1, 2, 3]");
    }

    #[test]
    fn test_negative_number() {
        let json = br#"{"mode": "test", "config": -123.45}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"-123.45");
    }

    #[test]
    fn test_scientific_notation() {
        let json = br#"{"mode": "test", "config": 1.5e10}"#;
        let scan = TaggedJsonScan::scan(json, "mode", "config").unwrap();

        let config = scan.content_bytes().unwrap();
        assert_eq!(config, b"1.5e10");
    }
}
