import * as icons from "lucide-static";
import { combineClassNames } from "lucide/dist/esm/replaceElement";
import { parser } from "posthtml-parser";
import * as changeCase from "change-case";

export default (options = {}) => tree => {
  // Accept options and set defaults
  // options.foo = options.foo || {}

  tree.parser = tree.parser || parser

  const process = node => {
    if (node.attrs != undefined && node.attrs["data-lucide"] != undefined) {
      let icon = changeCase.pascalCase(node.attrs["data-lucide"]);
      let iconSvg = icons[icon]

      if (!iconSvg) {
        console.warn("Icon not found: " + icon);
      }

      let iconNode = parser(iconSvg).find(n => n.tag === "svg")

      if (iconNode != undefined) {
        const iconAttrs = {
          ...iconNode.attrs,
          ...node.attrs,
        };

        const classNames = combineClassNames(["lucide-icon", iconNode.attrs?.class, node.attrs?.class]);

        if (classNames) {
          Object.assign(iconAttrs, {
            class: classNames,
          });
        }

        iconNode.attrs = iconAttrs;

        return iconNode
      }
    }

    // Return the node
    return node
  }

  return tree.walk(process)
}
