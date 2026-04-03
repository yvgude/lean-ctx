"use strict";
/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.text = text;
exports.otag = otag;
exports.ctag = ctag;
exports.element = element;
const errors_js_1 = require("./errors.js");
/**
 * Regular expression matching invalid XML 1.0 Unicode characters that must be removed.
 *
 * Based on the XML 1.0 specification (https://www.w3.org/TR/xml/#charsets):
 * - Control characters (U+0000-U+001F except tab, newline, carriage return)
 * - Delete character (U+007F)
 * - Invalid control characters (U+0080-U+009F except U+0085)
 * - Surrogate pairs (U+D800-U+DFFF)
 * - Non-characters (\p{NChar} - permanently reserved code points)
 *
 * Performance note: This regex uses Unicode property escapes and may be slower
 * on very large strings (100KB+). Consider pre-validation for untrusted input.
 *
 * @see https://www.w3.org/TR/xml/#charsets
 */
const invalidXMLUnicodeRegex = 
// eslint-disable-next-line no-control-regex
/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u0084\u0086-\u009F\uD800-\uDFFF\p{NChar}]/gu;
/**
 * Regular expressions for XML entity escaping
 */
const amp = /&/g;
const lt = /</g;
const gt = />/g;
const apos = /'/g;
const quot = /"/g;
/**
 * Valid XML attribute name pattern. XML names must:
 * - Start with a letter, underscore, or colon
 * - Contain only letters, digits, hyphens, underscores, colons, or periods
 *
 * This is a simplified validation that accepts the most common attribute names.
 * Note: In practice, this library only uses namespaced attributes like "video:title"
 * which are guaranteed to be valid.
 *
 * @see https://www.w3.org/TR/xml/#NT-Name
 */
const validAttributeNameRegex = /^[a-zA-Z_:][\w:.-]*$/;
/**
 * Validates that an attribute name is a valid XML identifier.
 *
 * XML attribute names must start with a letter, underscore, or colon,
 * and contain only alphanumeric characters, hyphens, underscores, colons, or periods.
 *
 * @param name - The attribute name to validate
 * @throws {InvalidXMLAttributeNameError} If the attribute name is invalid
 *
 * @example
 * validateAttributeName('href'); // OK
 * validateAttributeName('xml:lang'); // OK
 * validateAttributeName('data-value'); // OK
 * validateAttributeName('<script>'); // Throws InvalidXMLAttributeNameError
 */
function validateAttributeName(name) {
    if (!validAttributeNameRegex.test(name)) {
        throw new errors_js_1.InvalidXMLAttributeNameError(name);
    }
}
/**
 * Escapes text content for safe inclusion in XML text nodes.
 *
 * **Security Model:**
 * - Escapes `&` → `&amp;` (required to prevent entity interpretation)
 * - Escapes `<` → `&lt;` (required to prevent tag injection)
 * - Escapes `>` → `&gt;` (defense-in-depth, prevents CDATA injection)
 * - Does NOT escape `"` or `'` (not required in text content, only in attributes)
 * - Removes invalid XML Unicode characters per XML 1.0 spec
 *
 * **Why quotes aren't escaped:**
 * In XML text content (between tags), quotes have no special meaning and don't
 * need escaping. They only need escaping in attribute values, which is handled
 * by the `otag()` function.
 *
 * @param txt - The text content to escape
 * @returns XML-safe escaped text with invalid characters removed
 * @throws {TypeError} If txt is not a string
 *
 * @example
 * text('Hello & World'); // Returns: 'Hello &amp; World'
 * text('5 < 10'); // Returns: '5 &lt; 10'
 * text('Hello "World"'); // Returns: 'Hello "World"' (quotes OK in text)
 *
 * @see https://www.w3.org/TR/xml/#syntax
 */
function text(txt) {
    if (typeof txt !== 'string') {
        throw new TypeError(`text() requires a string, received ${typeof txt}: ${String(txt)}`);
    }
    return txt
        .replace(amp, '&amp;')
        .replace(lt, '&lt;')
        .replace(gt, '&gt;')
        .replace(invalidXMLUnicodeRegex, '');
}
/**
 * Generates an opening XML tag with optional attributes.
 *
 * **Security Model:**
 * - Validates attribute names to prevent injection via malformed names
 * - Escapes all attribute values with proper XML entity encoding
 * - Escapes `&`, `<`, `>`, `"`, and `'` in attribute values
 * - Removes invalid XML Unicode characters
 *
 * Attribute values use full escaping (including quotes) because they appear
 * within quoted strings in the XML output: `<tag attr="value">`.
 *
 * @param nodeName - The XML element name (e.g., 'url', 'loc', 'video:title')
 * @param attrs - Optional object mapping attribute names to string values
 * @param selfClose - If true, generates a self-closing tag (e.g., `<tag/>`)
 * @returns Opening XML tag string
 * @throws {InvalidXMLAttributeNameError} If an attribute name contains invalid characters
 * @throws {TypeError} If nodeName is not a string or attrs values are not strings
 *
 * @example
 * otag('url'); // Returns: '<url>'
 * otag('video:player_loc', { autoplay: 'ap=1' }); // Returns: '<video:player_loc autoplay="ap=1">'
 * otag('image:image', {}, true); // Returns: '<image:image/>'
 *
 * @see https://www.w3.org/TR/xml/#NT-Attribute
 */
function otag(nodeName, attrs, selfClose = false) {
    if (typeof nodeName !== 'string') {
        throw new TypeError(`otag() nodeName must be a string, received ${typeof nodeName}: ${String(nodeName)}`);
    }
    let attrstr = '';
    for (const k in attrs) {
        // Validate attribute name to prevent injection
        validateAttributeName(k);
        const attrValue = attrs[k];
        if (typeof attrValue !== 'string') {
            throw new TypeError(`otag() attribute "${k}" value must be a string, received ${typeof attrValue}: ${String(attrValue)}`);
        }
        // Escape attribute value with full entity encoding
        const val = attrValue
            .replace(amp, '&amp;')
            .replace(lt, '&lt;')
            .replace(gt, '&gt;')
            .replace(apos, '&apos;')
            .replace(quot, '&quot;')
            .replace(invalidXMLUnicodeRegex, '');
        attrstr += ` ${k}="${val}"`;
    }
    return `<${nodeName}${attrstr}${selfClose ? '/' : ''}>`;
}
/**
 * Generates a closing XML tag.
 *
 * @param nodeName - The XML element name (e.g., 'url', 'loc', 'video:title')
 * @returns Closing XML tag string
 * @throws {TypeError} If nodeName is not a string
 *
 * @example
 * ctag('url'); // Returns: '</url>'
 * ctag('video:title'); // Returns: '</video:title>'
 */
function ctag(nodeName) {
    if (typeof nodeName !== 'string') {
        throw new TypeError(`ctag() nodeName must be a string, received ${typeof nodeName}: ${String(nodeName)}`);
    }
    return `</${nodeName}>`;
}
function element(nodeName, attrs, innerText) {
    if (typeof attrs === 'string') {
        // Pattern 1: element(nodeName, textContent)
        return otag(nodeName) + text(attrs) + ctag(nodeName);
    }
    else if (innerText !== undefined) {
        // Pattern 2: element(nodeName, attrs, textContent)
        return otag(nodeName, attrs) + text(innerText) + ctag(nodeName);
    }
    else {
        // Pattern 3: element(nodeName, attrs) - self-closing
        return otag(nodeName, attrs, true);
    }
}
