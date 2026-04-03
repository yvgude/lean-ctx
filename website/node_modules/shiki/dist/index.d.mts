import { a as bundledLanguagesInfo, i as bundledLanguagesBase, n as bundledLanguages, r as bundledLanguagesAlias, t as BundledLanguage } from "./langs-bundle-full-C-zczmvu.mjs";
import { BundledTheme, bundledThemes, bundledThemesInfo } from "./themes.mjs";
import { Highlighter, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter, t as bundle_full_d_exports } from "./bundle-full.mjs";
import { BuiltinLanguage, BuiltinTheme } from "./types.mjs";
import { createOnigurumaEngine, loadWasm } from "@shikijs/engine-oniguruma";
import { createJavaScriptRegexEngine, defaultJavaScriptRegexConstructor } from "@shikijs/engine-javascript";
export * from "@shikijs/core";

//#region src/index.d.ts
declare namespace index_d_exports {
  export { BuiltinLanguage, BuiltinTheme, BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, createJavaScriptRegexEngine, createOnigurumaEngine, defaultJavaScriptRegexConstructor, getLastGrammarState, getSingletonHighlighter, loadWasm };
}
//#endregion
export { type BuiltinLanguage, type BuiltinTheme, BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, createJavaScriptRegexEngine, createOnigurumaEngine, defaultJavaScriptRegexConstructor, getLastGrammarState, getSingletonHighlighter, loadWasm };