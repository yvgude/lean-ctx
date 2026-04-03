import { PatternScanner, RegexEngineString } from "@shikijs/types";
import { IOnigMatch } from "@shikijs/vscode-textmate";

//#region src/scanner.d.ts
interface JavaScriptRegexScannerOptions {
  /**
   * Whether to allow invalid regex patterns.
   *
   * @default false
   */
  forgiving?: boolean;
  /**
   * Cache for regex patterns.
   */
  cache?: Map<string, RegExp | Error> | null;
  /**
   * Custom pattern to RegExp constructor.
   *
   * By default `oniguruma-to-es` is used.
   */
  regexConstructor?: (pattern: string) => RegExp;
}
declare class JavaScriptScanner implements PatternScanner {
  patterns: (string | RegExp)[];
  options: JavaScriptRegexScannerOptions;
  regexps: (RegExp | null)[];
  constructor(patterns: (string | RegExp)[], options?: JavaScriptRegexScannerOptions);
  findNextMatchSync(string: string | RegexEngineString, startPosition: number, _options: number): IOnigMatch | null;
}
//#endregion
export { JavaScriptScanner as n, JavaScriptRegexScannerOptions as t };