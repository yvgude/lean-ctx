import { n as __reExport, t as __exportAll } from "./chunk-CtajNgzt.mjs";
import { i as bundledLanguagesInfo, n as bundledLanguagesAlias, r as bundledLanguagesBase, t as bundledLanguages } from "./langs-bundle-full-DfKZStlK.mjs";
import { bundledThemes, bundledThemesInfo } from "./themes.mjs";
import { codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, getLastGrammarState, getSingletonHighlighter, t as bundle_full_exports } from "./bundle-full.mjs";
import { createOnigurumaEngine, loadWasm } from "@shikijs/engine-oniguruma";
import { createJavaScriptRegexEngine, defaultJavaScriptRegexConstructor } from "@shikijs/engine-javascript";

export * from "@shikijs/core"

//#region src/index.ts
var src_exports = /* @__PURE__ */ __exportAll({
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
	createJavaScriptRegexEngine: () => createJavaScriptRegexEngine,
	createOnigurumaEngine: () => createOnigurumaEngine,
	defaultJavaScriptRegexConstructor: () => defaultJavaScriptRegexConstructor,
	getLastGrammarState: () => getLastGrammarState,
	getSingletonHighlighter: () => getSingletonHighlighter,
	loadWasm: () => loadWasm
});
__reExport(src_exports, bundle_full_exports);

//#endregion
export { bundledLanguages, bundledLanguagesAlias, bundledLanguagesBase, bundledLanguagesInfo, bundledThemes, bundledThemesInfo, codeToHast, codeToHtml, codeToTokens, codeToTokensBase, codeToTokensWithThemes, createHighlighter, createJavaScriptRegexEngine, createOnigurumaEngine, defaultJavaScriptRegexConstructor, getLastGrammarState, getSingletonHighlighter, loadWasm };