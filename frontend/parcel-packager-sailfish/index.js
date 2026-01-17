import {Packager} from '@parcel/plugin';
import { default as html_packager } from "@parcel/packager-html";

// @flow
// Pulled from https://github.com/parcel-bundler/parcel/blob/73f691d67d22482440babb2d1846b7da2160f7cc/packages/core/utils/src/escape-html.js#L14
// Based on _.escape https://github.com/lodash/lodash/blob/master/escape.js
const reUnescapedHtml = /&(amp|lt|gt|quot|#39);/g;
const reHasUnescapedHtml = RegExp(reUnescapedHtml.source);

const htmlUnescapes = {
  '&amp;': '&',
  '&lt;': '<',
  '&gt;': '>',
  '&quot;': '"',
  '&#39;': "'",
};

function unescapeHTML(s) {
  if (reHasUnescapedHtml.test(s)) {
    return s.replace(reUnescapedHtml, c => htmlUnescapes[c]);
  }

  return s;
}

// Gemini coded this
function getInternalPackageFn(wrapperObject) {
  // 1. Safety check: ensure the object and the 'default' property exist
  if (!wrapperObject || !wrapperObject.default) {
    console.warn("Invalid object structure: 'default' property missing.");
    return null;
  }

  const packagerInstance = wrapperObject.default;

  // 2. Get all symbol keys from the instance
  const symbols = Object.getOwnPropertySymbols(packagerInstance);

  // 3. Find the symbol specifically named 'parcel-plugin-config'
  const configSymbol = symbols.find(
    (sym) => sym.description === "parcel-plugin-config"
  );

  if (!configSymbol) {
    console.warn("Could not find the 'parcel-plugin-config' symbol.");
    return null;
  }

  // 4. Access the hidden config object
  const internalConfig = packagerInstance[configSymbol];

  // 5. Return the package function if it exists
  return internalConfig?.package || null;
}

export default new Packager({
  async package(args) {
    const packageFn = getInternalPackageFn(html_packager);
    let bundle = await packageFn(args)

    bundle.contents = bundle.contents.toString('utf8')
      .replace('<html><head></head><body>', '')
      .replace('</body></html>', '')
      .replaceAll('&lt;%%', '<%')
      .replace(/&lt;%[^%]*%&gt;/g, c => unescapeHTML(c))
      .replace(/__prop__="([^"]*)"/g, '$1')
    return bundle;
  }
});
