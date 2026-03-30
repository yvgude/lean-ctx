import { BundledTheme, bundledThemes, bundledThemesInfo } from "./themes.mjs";
import * as _shikijs_types0 from "@shikijs/types";
import { BundledLanguageInfo, DynamicImportLanguageRegistration, HighlighterGeneric } from "@shikijs/types";
import * as hast from "hast";
export * from "@shikijs/core";

//#region src/langs-bundle-web.d.ts
declare const bundledLanguagesInfo: BundledLanguageInfo[];
declare const bundledLanguagesBase: {
  [k: string]: DynamicImportLanguageRegistration;
};
declare const bundledLanguagesAlias: {
  [k: string]: DynamicImportLanguageRegistration;
};
type BundledLanguage = 'angular-html' | 'angular-ts' | 'astro' | 'bash' | 'blade' | 'c' | 'c++' | 'cjs' | 'coffee' | 'coffeescript' | 'cpp' | 'css' | 'csv' | 'cts' | 'glsl' | 'gql' | 'graphql' | 'haml' | 'handlebars' | 'hbs' | 'html' | 'html-derivative' | 'http' | 'hurl' | 'imba' | 'jade' | 'java' | 'javascript' | 'jinja' | 'jison' | 'jl' | 'js' | 'json' | 'json5' | 'jsonc' | 'jsonl' | 'jsx' | 'julia' | 'less' | 'lit' | 'markdown' | 'marko' | 'md' | 'mdc' | 'mdx' | 'mjs' | 'mts' | 'php' | 'postcss' | 'pug' | 'py' | 'python' | 'r' | 'regex' | 'regexp' | 'sass' | 'scss' | 'sh' | 'shell' | 'shellscript' | 'sql' | 'styl' | 'stylus' | 'svelte' | 'ts' | 'ts-tags' | 'tsx' | 'typescript' | 'vue' | 'vue-html' | 'vue-vine' | 'wasm' | 'wgsl' | 'wit' | 'xml' | 'yaml' | 'yml' | 'zsh';
declare const bundledLanguages: Record<BundledLanguage, DynamicImportLanguageRegistration>;
declare namespace bundle_web_d_exports {
  export { BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter };
}
type Highlighter = HighlighterGeneric<BundledLanguage, BundledTheme>;
/**
 * Initiate a highlighter instance and load the specified languages and themes.
 * Later it can be used synchronously to highlight code.
 *
 * Importing this function will bundle all languages and themes.
 * @see https://shiki.style/guide/bundles#shiki-bundle-web
 *
 * For granular control over the bundle, check:
 * @see https://shiki.style/guide/bundles#fine-grained-bundle
 */
declare const createHighlighter: _shikijs_types0.CreateHighlighterFactory<BundledLanguage, BundledTheme>;
declare const codeToHtml: (code: string, options: _shikijs_types0.CodeToHastOptions<BundledLanguage, BundledTheme>) => Promise<string>, codeToHast: (code: string, options: _shikijs_types0.CodeToHastOptions<BundledLanguage, BundledTheme>) => Promise<hast.Root>, codeToTokensBase: (code: string, options: _shikijs_types0.RequireKeys<_shikijs_types0.CodeToTokensBaseOptions<BundledLanguage, BundledTheme>, "theme" | "lang">) => Promise<_shikijs_types0.ThemedToken[][]>, codeToTokens: (code: string, options: _shikijs_types0.CodeToTokensOptions<BundledLanguage, BundledTheme>) => Promise<_shikijs_types0.TokensResult>, codeToTokensWithThemes: (code: string, options: _shikijs_types0.RequireKeys<_shikijs_types0.CodeToTokensWithThemesOptions<BundledLanguage, BundledTheme>, "lang" | "themes">) => Promise<_shikijs_types0.ThemedTokenWithVariants[][]>, getSingletonHighlighter: (options?: Partial<_shikijs_types0.BundledHighlighterOptions<BundledLanguage, BundledTheme>> | undefined) => Promise<HighlighterGeneric<BundledLanguage, BundledTheme>>, getLastGrammarState: ((element: _shikijs_types0.ThemedToken[][] | hast.Root) => _shikijs_types0.GrammarState) | ((code: string, options: _shikijs_types0.CodeToTokensBaseOptions<BundledLanguage, BundledTheme>) => Promise<_shikijs_types0.GrammarState>);
//#endregion
export { BundledLanguage, BundledTheme, Highlighter, bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter };