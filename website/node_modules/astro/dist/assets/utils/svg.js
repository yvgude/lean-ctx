import { optimize } from "svgo";
import { ELEMENT_NODE, TEXT_NODE, parse, renderSync } from "ultrahtml";
import { AstroError, AstroErrorData } from "../../core/errors/index.js";
import { dropAttributes } from "../runtime.js";
function parseSvg({
  path,
  contents,
  svgoConfig
}) {
  let processedContents = contents;
  if (svgoConfig) {
    try {
      const config = typeof svgoConfig === "boolean" ? void 0 : svgoConfig;
      const result = optimize(contents, config);
      processedContents = result.data;
    } catch (cause) {
      throw new AstroError(
        {
          ...AstroErrorData.CannotOptimizeSvg,
          message: AstroErrorData.CannotOptimizeSvg.message(path)
        },
        { cause }
      );
    }
  }
  const root = parse(processedContents);
  const svgNode = root.children.find(
    ({ name, type }) => type === ELEMENT_NODE && name === "svg"
  );
  if (!svgNode) {
    throw new Error("SVG file does not contain an <svg> element");
  }
  const { attributes, children } = svgNode;
  const body = renderSync({ ...root, children });
  const styles = [];
  for (const child of children) {
    if (child.type === ELEMENT_NODE && child.name === "style") {
      const textContent = child.children?.filter((c) => c.type === TEXT_NODE).map((c) => c.value).join("");
      if (textContent) {
        styles.push(textContent);
      }
    }
  }
  return { attributes, body, styles };
}
function makeSvgComponent(meta, contents, svgoConfig) {
  const file = typeof contents === "string" ? contents : contents.toString("utf-8");
  const {
    attributes,
    body: children,
    styles
  } = parseSvg({
    path: meta.fsPath,
    contents: file,
    svgoConfig
  });
  const props = {
    meta,
    attributes: dropAttributes(attributes),
    children,
    styles
  };
  return `import { createSvgComponent } from 'astro/assets/runtime';
export default createSvgComponent(${JSON.stringify(props)})`;
}
function parseSvgComponentData(meta, contents, svgoConfig) {
  const file = typeof contents === "string" ? contents : contents.toString("utf-8");
  const {
    attributes,
    body: children,
    styles
  } = parseSvg({
    path: meta.fsPath,
    contents: file,
    svgoConfig
  });
  return { attributes: dropAttributes(attributes), children, styles };
}
export {
  makeSvgComponent,
  parseSvgComponentData
};
