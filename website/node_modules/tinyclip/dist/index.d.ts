/**
 * Reads text from the clipboard.
 */
export declare function readText(): Promise<string>;
/**
 * Writes text to the clipboard.
 */
export declare function writeText(text: string): Promise<void>;
