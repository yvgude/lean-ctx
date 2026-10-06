// SPDX-License-Identifier: Apache-2.0
// `Navigator` backed by the editor's own language features — whatever
// language extensions are installed (TypeScript is built in; rust-analyzer,
// Python, Go, Java, C# … come with their extensions).

import * as vscode from "vscode";
import { Direction, Location, Navigator, Position, TypeNode } from "./protocol";

const toPosition = (p: Position) => new vscode.Position(p.line, p.character);

const toRange = (r: vscode.Range) => ({
  start: { line: r.start.line, character: r.start.character },
  end: { line: r.end.line, character: r.end.character },
});

/** Locations on disk; virtual documents (untitled, git, …) have no file. */
function locations(found: (vscode.Location | vscode.LocationLink)[] | undefined): Location[] {
  const out: Location[] = [];
  for (const l of found ?? []) {
    const uri = "targetUri" in l ? l.targetUri : l.uri;
    const range = "targetUri" in l ? (l.targetSelectionRange ?? l.targetRange) : l.range;
    if (uri.scheme === "file") out.push({ file: uri.fsPath, range: toRange(range) });
  }
  return out;
}

function typeNode(item: vscode.TypeHierarchyItem): TypeNode | null {
  if (item.uri.scheme !== "file") return null;
  return { name: item.name, file: item.uri.fsPath, line: item.selectionRange.start.line };
}

async function provider(command: string, file: string, pos: Position): Promise<Location[]> {
  return locations(
    await vscode.commands.executeCommand<(vscode.Location | vscode.LocationLink)[]>(
      command,
      vscode.Uri.file(file),
      toPosition(pos),
    ),
  );
}

export const vscodeNavigator: Navigator = {
  definition: (file, pos) => provider("vscode.executeDefinitionProvider", file, pos),
  declaration: (file, pos) => provider("vscode.executeDeclarationProvider", file, pos),
  references: (file, pos) => provider("vscode.executeReferenceProvider", file, pos),
  implementations: (file, pos) => provider("vscode.executeImplementationProvider", file, pos),
  async typeHierarchy(file: string, pos: Position, direction: Direction) {
    const items = await vscode.commands.executeCommand<vscode.TypeHierarchyItem[]>(
      "vscode.prepareTypeHierarchy",
      vscode.Uri.file(file),
      toPosition(pos),
    );
    const item = items?.[0];
    const root = item && typeNode(item);
    if (!item || !root) return null;
    const related =
      (await vscode.commands.executeCommand<vscode.TypeHierarchyItem[]>(
        direction === "supertypes" ? "vscode.provideSupertypes" : "vscode.provideSubtypes",
        item,
      )) ?? [];
    return { root, related: related.map(typeNode).filter((n): n is TypeNode => n !== null) };
  },
};
