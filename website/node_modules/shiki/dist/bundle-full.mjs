import { n as __reExport, t as __exportAll } from "./chunk-CtajNgzt.mjs";
import { t as engine_oniguruma_exports } from "./engine-oniguruma.mjs";
import { i as bundledLanguagesInfo, n as bundledLanguagesAlias, r as bundledLanguagesBase, t as bundledLanguages } from "./langs-bundle-full-DfKZStlK.mjs";
import { bundledThemes, bundledThemesInfo } from "./themes.mjs";
import { createBundledHighlighter, createSingletonShorthands, guessEmbeddedLanguages } from "@shikijs/core";

export * from "@shikijs/core"

//#region src/bundle-full.ts
var bundle_full_exports = /* @__PURE__ */ __exportAll({
	bundledLanguages: () => bundledLanguages,
	bundledLanguagesAlias: () => bundledLanguagesAlias,
	bundledLanguagesBase: () => bundledLanguagesBase,
	bundledLanguagesInfo: () => bundledLanguagesInfo,
	bundledThemes: () => bundledThemes,
	bundledThemesInfo: () => bundledThemesInfo,
	codeToHast: () => codeToHast,
	codeToHtml: () => codeToHtml,
	codeToTokens: () => codeToTokens,
	codeToTokensBase: () => codeToTokensBase,
	codeToTokensWithThemes: () => codeToTokensWithThemes,
	createHighlighter: () => createHighlighter,
	getLastGrammarState: () => getLastGrammarState,
	getSingletonHighlighter: () => getSingletonHighlighter
});
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
const createHighlighter = /* @__PURE__ */ createBundledHighlighter({
	langs: bundledLanguages,
	themes: bundledThemes,
	engine: () => (0, engine_oniguruma_exports.createOnigurumaEngine)(import("shiki/wasm"))
});
const { codeToHtml, codeToHast, codeToTokens, codeToTokensBase, codeToTokensWithThemes, getSingletonHighlighter, getLastGrammarState } = /* @__PURE__ */ createSingletonShorthands(createHighlighter, { guessEmbeddedLanguages });

//#endregion
export { bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter, bundle_full_exports as t };