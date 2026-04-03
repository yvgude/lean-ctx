import { C as ProxifiedValue, D as detectCodeFormat, E as CodeFormatOptions, O as Options, S as ProxifiedObject, T as ProxyType, _ as ProxifiedImportsMap, a as Token, b as ProxifiedModule, c as Proxified, d as ProxifiedBinaryExpression, f as ProxifiedBlockStatement, g as ProxifiedImportItem, h as ProxifiedIdentifier, i as ParsedFileNode, l as ProxifiedArray, m as ProxifiedFunctionExpression, n as GenerateOptions, o as BinaryOperator, p as ProxifiedFunctionCall, r as Loc, s as ImportItemInput, t as ASTNode, u as ProxifiedArrowFunctionExpression, v as ProxifiedLogicalExpression, w as ProxyBase, x as ProxifiedNewExpression, y as ProxifiedMemberExpression } from "./types-r4mG9WUV.js";
import { MagicastError, MagicastErrorOptions, builders, generateCode, parseExpression, parseModule } from "./core.js";

//#region src/file.d.ts
declare function loadFile<Exports extends object = any>(filename: string, options?: Options): Promise<ProxifiedModule<Exports>>;
declare function writeFile(node: {
  $ast: ASTNode;
} | ASTNode, filename: string, options?: Options): Promise<void>;
//#endregion
export { ASTNode, BinaryOperator, CodeFormatOptions, GenerateOptions, ImportItemInput, Loc, MagicastError, MagicastErrorOptions, ParsedFileNode, Proxified, ProxifiedArray, ProxifiedArrowFunctionExpression, ProxifiedBinaryExpression, ProxifiedBlockStatement, ProxifiedFunctionCall, ProxifiedFunctionExpression, ProxifiedIdentifier, ProxifiedImportItem, ProxifiedImportsMap, ProxifiedLogicalExpression, ProxifiedMemberExpression, ProxifiedModule, ProxifiedNewExpression, ProxifiedObject, ProxifiedValue, ProxyBase, ProxyType, Token, builders, detectCodeFormat, generateCode, loadFile, parseExpression, parseModule, writeFile };