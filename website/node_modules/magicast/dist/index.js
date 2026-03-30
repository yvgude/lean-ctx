import { a as detectCodeFormat, i as parseModule, n as generateCode, o as MagicastError, r as parseExpression, t as builders } from "./builders-B_BPSg0Q.js";
import "./core.js";
import { promises } from "node:fs";

//#region src/file.ts
async function loadFile(filename, options = {}) {
	const contents = await promises.readFile(filename, "utf8");
	options.sourceFileName = options.sourceFileName ?? filename;
	return parseModule(contents, options);
}
async function writeFile(node, filename, options) {
	const { code, map } = generateCode("$ast" in node ? node.$ast : node, options);
	await promises.writeFile(filename, code);
	if (map) await promises.writeFile(filename + ".map", map);
}

//#endregion
export { MagicastError, builders, detectCodeFormat, generateCode, loadFile, parseExpression, parseModule, writeFile };