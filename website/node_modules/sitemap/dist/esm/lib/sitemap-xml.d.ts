/*!
 * Sitemap
 * Copyright(c) 2011 Eugene Kalinin
 * MIT Licensed
 */
import { TagNames, IndexTagNames, StringObj } from './types.js';
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
export declare function text(txt: string): string;
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
export declare function otag(nodeName: TagNames | IndexTagNames, attrs?: StringObj, selfClose?: boolean): string;
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
export declare function ctag(nodeName: TagNames | IndexTagNames): string;
/**
 * Generates a complete XML element with optional attributes and text content.
 *
 * This is a convenience function that combines `otag()`, `text()`, and `ctag()`.
 * It supports three usage patterns via function overloading:
 *
 * 1. Element with text content: `element('loc', 'https://example.com')`
 * 2. Element with attributes and text: `element('video:player_loc', { autoplay: 'ap=1' }, 'https://...')`
 * 3. Self-closing element with attributes: `element('image:image', { href: '...' })`
 *
 * @param nodeName - The XML element name
 * @param attrs - Either a string (text content) or object (attributes)
 * @param innerText - Optional text content when attrs is an object
 * @returns Complete XML element string
 * @throws {InvalidXMLAttributeNameError} If an attribute name contains invalid characters
 * @throws {TypeError} If arguments have invalid types
 *
 * @example
 * // Pattern 1: Simple element with text
 * element('loc', 'https://example.com')
 * // Returns: '<loc>https://example.com</loc>'
 *
 * @example
 * // Pattern 2: Element with attributes and text
 * element('video:player_loc', { autoplay: 'ap=1' }, 'https://example.com/video')
 * // Returns: '<video:player_loc autoplay="ap=1">https://example.com/video</video:player_loc>'
 *
 * @example
 * // Pattern 3: Self-closing element with attributes
 * element('xhtml:link', { rel: 'alternate', href: 'https://example.com/fr' })
 * // Returns: '<xhtml:link rel="alternate" href="https://example.com/fr"/>'
 */
export declare function element(nodeName: TagNames, attrs: StringObj, innerText: string): string;
export declare function element(nodeName: TagNames | IndexTagNames, innerText: string): string;
export declare function element(nodeName: TagNames, attrs: StringObj): string;
