import { a as bundledLanguagesInfo, i as bundledLanguagesBase, n as bundledLanguages, r as bundledLanguagesAlias, t as BundledLanguage } from "./langs-bundle-full-C-zczmvu.mjs";
import { BundledTheme, bundledThemes, bundledThemesInfo } from "./themes.mjs";
import * as _shikijs_types0 from "@shikijs/types";
import { HighlighterGeneric } from "@shikijs/types";
import * as hast from "hast";
export * from "@shikijs/core";

//#region src/bundle-full.d.ts
declare namespace bundle_full_d_exports {
  export { BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter };
}
type Highlighter = HighlighterGeneric<BundledLanguage, BundledTheme>;
/**
 * Initiate a highlighter instance and load the specified languages and themes.
 * Later it can be used synchronously to highlight code.
 *
 * Importing this function will bundle all languages and themes.
 * @see https://shiki.style/guide/bundles#shiki-bundle-full
 *
 * For granular control over the bundle, check:
 * @see https://shiki.style/guide/bundles#fine-grained-bundle
 */
declare const createHighlighter: _shikijs_types0.CreateHighlighterFactory<BundledLanguage, BundledTheme>;
declare const codeToHtml: (code: string, options: _shikijs_types0.CodeToHastOptions<BundledLanguage, BundledTheme>) => Promise<string>, codeToHast: (code: string, options: _shikijs_types0.CodeToHastOptions<BundledLanguage, BundledTheme>) => Promise<hast.Root>, codeToTokens: (code: string, options: _shikijs_types0.CodeToTokensOptions<BundledLanguage, BundledTheme>) => Promise<_shikijs_types0.TokensResult>, codeToTokensBase: (code: string, options: _shikijs_types0.RequireKeys<_shikijs_types0.CodeToTokensBaseOptions<BundledLanguage, BundledTheme>, "theme" | "lang">) => Promise<_shikijs_types0.ThemedToken[][]>, codeToTokensWithThemes: (code: string, options: _shikijs_types0.RequireKeys<_shikijs_types0.CodeToTokensWithThemesOptions<BundledLanguage, BundledTheme>, "lang" | "themes">) => Promise<_shikijs_types0.ThemedTokenWithVariants[][]>, getSingletonHighlighter: (options?: Partial<_shikijs_types0.BundledHighlighterOptions<BundledLanguage, BundledTheme>> | undefined) => Promise<HighlighterGeneric<BundledLanguage, BundledTheme>>, getLastGrammarState: ((element: _shikijs_types0.ThemedToken[][] | hast.Root) => _shikijs_types0.GrammarState) | ((code: string, options: _shikijs_types0.CodeToTokensBaseOptions<BundledLanguage, BundledTheme>) => Promise<_shikijs_types0.GrammarState>);
//#endregion
export { BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter, bundle_full_d_exports as t };