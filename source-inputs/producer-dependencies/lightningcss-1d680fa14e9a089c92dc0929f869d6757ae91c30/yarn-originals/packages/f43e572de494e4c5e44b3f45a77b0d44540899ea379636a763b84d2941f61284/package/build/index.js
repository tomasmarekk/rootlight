"use strict";

Object.defineProperty(exports, "__esModule", {
  value: true
});
exports.default = markdownItPrism;

var _prismjs = _interopRequireDefault(require("prismjs"));

var _components = _interopRequireDefault(require("prismjs/components/"));

function _interopRequireDefault(obj) { return obj && obj.__esModule ? obj : { default: obj }; }

function ownKeys(object, enumerableOnly) { var keys = Object.keys(object); if (Object.getOwnPropertySymbols) { var symbols = Object.getOwnPropertySymbols(object); enumerableOnly && (symbols = symbols.filter(function (sym) { return Object.getOwnPropertyDescriptor(object, sym).enumerable; })), keys.push.apply(keys, symbols); } return keys; }

function _objectSpread(target) { for (var i = 1; i < arguments.length; i++) { var source = null != arguments[i] ? arguments[i] : {}; i % 2 ? ownKeys(Object(source), !0).forEach(function (key) { _defineProperty(target, key, source[key]); }) : Object.getOwnPropertyDescriptors ? Object.defineProperties(target, Object.getOwnPropertyDescriptors(source)) : ownKeys(Object(source)).forEach(function (key) { Object.defineProperty(target, key, Object.getOwnPropertyDescriptor(source, key)); }); } return target; }

function _defineProperty(obj, key, value) { if (key in obj) { Object.defineProperty(obj, key, { value: value, enumerable: true, configurable: true, writable: true }); } else { obj[key] = value; } return obj; }

function _slicedToArray(arr, i) { return _arrayWithHoles(arr) || _iterableToArrayLimit(arr, i) || _unsupportedIterableToArray(arr, i) || _nonIterableRest(); }

function _nonIterableRest() { throw new TypeError("Invalid attempt to destructure non-iterable instance.\nIn order to be iterable, non-array objects must have a [Symbol.iterator]() method."); }

function _iterableToArrayLimit(arr, i) { var _i = arr == null ? null : typeof Symbol !== "undefined" && arr[Symbol.iterator] || arr["@@iterator"]; if (_i == null) return; var _arr = []; var _n = true; var _d = false; var _s, _e; try { for (_i = _i.call(arr); !(_n = (_s = _i.next()).done); _n = true) { _arr.push(_s.value); if (i && _arr.length === i) break; } } catch (err) { _d = true; _e = err; } finally { try { if (!_n && _i["return"] != null) _i["return"](); } finally { if (_d) throw _e; } } return _arr; }

function _arrayWithHoles(arr) { if (Array.isArray(arr)) return arr; }

function _createForOfIteratorHelper(o, allowArrayLike) { var it = typeof Symbol !== "undefined" && o[Symbol.iterator] || o["@@iterator"]; if (!it) { if (Array.isArray(o) || (it = _unsupportedIterableToArray(o)) || allowArrayLike && o && typeof o.length === "number") { if (it) o = it; var i = 0; var F = function F() {}; return { s: F, n: function n() { if (i >= o.length) return { done: true }; return { done: false, value: o[i++] }; }, e: function e(_e2) { throw _e2; }, f: F }; } throw new TypeError("Invalid attempt to iterate non-iterable instance.\nIn order to be iterable, non-array objects must have a [Symbol.iterator]() method."); } var normalCompletion = true, didErr = false, err; return { s: function s() { it = it.call(o); }, n: function n() { var step = it.next(); normalCompletion = step.done; return step; }, e: function e(_e3) { didErr = true; err = _e3; }, f: function f() { try { if (!normalCompletion && it.return != null) it.return(); } finally { if (didErr) throw err; } } }; }

function _unsupportedIterableToArray(o, minLen) { if (!o) return; if (typeof o === "string") return _arrayLikeToArray(o, minLen); var n = Object.prototype.toString.call(o).slice(8, -1); if (n === "Object" && o.constructor) n = o.constructor.name; if (n === "Map" || n === "Set") return Array.from(o); if (n === "Arguments" || /^(?:Ui|I)nt(?:8|16|32)(?:Clamped)?Array$/.test(n)) return _arrayLikeToArray(o, minLen); }

function _arrayLikeToArray(arr, len) { if (len == null || len > arr.length) len = arr.length; for (var i = 0, arr2 = new Array(len); i < len; i++) arr2[i] = arr[i]; return arr2; }

const SPECIFIED_LANGUAGE_META_KEY = 'de.joshuagleitze.markdown-it-prism.specifiedLanguage';
const DEFAULTS = {
  highlightInlineCode: false,
  plugins: [],
  init: () => {// do nothing by default
  },
  defaultLanguageForUnknown: undefined,
  defaultLanguageForUnspecified: undefined,
  defaultLanguage: undefined
};
/**
 * Loads the provided `lang` into prism.
 *
 * @param lang
 *        Code of the language to load.
 * @return The Prism language object for the provided {@code lang} code. {@code undefined} if the language is not known to Prism.
 */

function loadPrismLang(lang) {
  if (!lang) return undefined;
  let langObject = _prismjs.default.languages[lang];

  if (langObject === undefined) {
    (0, _components.default)([lang]);
    langObject = _prismjs.default.languages[lang];
  }

  return langObject;
}
/**
 * Loads the provided Prism plugin.
 * @param name
 *        Name of the plugin to load.
 * @throws {Error} If there is no plugin with the provided `name`.
 */


function loadPrismPlugin(name) {
  try {
    require(`prismjs/plugins/${name}/prism-${name}`);
  } catch (e) {
    throw new Error(`Cannot load Prism plugin "${name}". Please check the spelling.`);
  }
}
/**
 * Select the language to use for highlighting, based on the provided options and the specified language.
 *
 * @param options
 *        The options that were used to initialise the plugin.
 * @param lang
 *        Code of the language to highlight the text in.
 * @return The name of the language to use and the Prism language object for that language.
 */


function selectLanguage(options, lang) {
  let langToUse = lang;

  if (langToUse === '' && options.defaultLanguageForUnspecified !== undefined) {
    langToUse = options.defaultLanguageForUnspecified;
  }

  let prismLang = loadPrismLang(langToUse);

  if (prismLang === undefined && options.defaultLanguageForUnknown !== undefined) {
    langToUse = options.defaultLanguageForUnknown;
    prismLang = loadPrismLang(langToUse);
  }

  return [langToUse, prismLang];
}
/**
 * Highlights the provided text using Prism.
 *
 * @param markdownit
 *        The markdown-it instance.
 * @param options
 *        The options that have been used to initialise the plugin.
 * @param text
 *        The text to highlight.
 * @param lang
 *        Code of the language to highlight the text in.
 * @return If Prism knows the language that {@link selectLanguage} returns for `lang`, the `text` highlighted for that language. Otherwise, `text`
 *  html-escaped.
 */


function highlight(markdownit, options, text, lang) {
  return highlightWithSelectedLanguage(markdownit, options, text, selectLanguage(options, lang));
}
/**
 * Highlights the provided text using Prism.
 *
 * @param markdownit
 *        The markdown-it instance.
 * @param options
 *        The options that have been used to initialise the plugin.
 * @param text
 *        The text to highlight.
 * @param lang
 *        The selected Prism language to use for highlighting.
 * @return If Prism knows the language that {@link selectLanguage} returns for `lang`, the `text` highlighted for that language. Otherwise, `text`
 *  html-escaped.
 */


function highlightWithSelectedLanguage(markdownit, options, text, [langToUse, prismLang]) {
  return prismLang ? _prismjs.default.highlight(text, prismLang, langToUse) : markdownit.utils.escapeHtml(text);
}
/**
 * Construct the class name for the provided `lang`.
 *
 * @param markdownit
 *        The markdown-it instance.
 * @param lang
 *        The selected language.
 * @return the class to use for `lang`.
 */


function languageClass(markdownit, lang) {
  return markdownit.options.langPrefix + lang;
}
/**
 * A {@link RuleCore} that searches for and extracts language specifications on inline code tokens.
 */


function inlineCodeLanguageRule(state) {
  var _iterator = _createForOfIteratorHelper(state.tokens),
      _step;

  try {
    for (_iterator.s(); !(_step = _iterator.n()).done;) {
      const inlineToken = _step.value;

      if (inlineToken.type === 'inline' && inlineToken.children !== null) {
        var _iterator2 = _createForOfIteratorHelper(inlineToken.children.entries()),
            _step2;

        try {
          for (_iterator2.s(); !(_step2 = _iterator2.n()).done;) {
            const _step2$value = _slicedToArray(_step2.value, 2),
                  index = _step2$value[0],
                  token = _step2$value[1];

            if (token.type === 'code_inline' && index + 1 < inlineToken.children.length) {
              extractInlineCodeSpecifiedLanguage(token, inlineToken.children[index + 1]);
            }
          }
        } catch (err) {
          _iterator2.e(err);
        } finally {
          _iterator2.f();
        }
      }
    }
  } catch (err) {
    _iterator.e(err);
  } finally {
    _iterator.f();
  }
}
/**
 * Searches for a language specification after an inline code token (e.g. ``{language=cpp}). If present, extracts the language, sets
 * it on `inlineCodeToken`’s meta, and removes the specification.
 *
 * @param inlineCodeToken
 *  The inline code token for which to extract the language.
 * @param followingToken
 *    The token immediately following the `inlineCodeToken`.
 */


function extractInlineCodeSpecifiedLanguage(inlineCodeToken, followingToken) {
  const languageSpecificationMatch = followingToken.content.match(/^\{((?:[^\s}]+\s)*)language=([^\s}]+)((?:\s[^\s}]+)*)}/);

  if (languageSpecificationMatch !== null) {
    inlineCodeToken.meta = _objectSpread(_objectSpread({}, inlineCodeToken.meta), {}, {
      [SPECIFIED_LANGUAGE_META_KEY]: languageSpecificationMatch[2]
    });
    followingToken.content = followingToken.content.slice(languageSpecificationMatch[0].length);

    if (languageSpecificationMatch[1] || languageSpecificationMatch[3]) {
      followingToken.content = `{${languageSpecificationMatch[1] || ''}${(languageSpecificationMatch[3] || ' ').slice(1)}}${followingToken.content}`;
    }
  }
}
/**
 * Patch the `<pre>` and `<code>` tags produced by the `existingRule` for fenced code blocks.
 *
 * @param markdownit
 *        The markdown-it instance.
 * @param options
 *        The options that have been used to initialise the plugin.
 * @param existingRule
 *        The previously configured render rule for fenced code blocks.
 */


function applyCodeAttributes(markdownit, options, existingRule) {
  return (tokens, idx, renderOptions, env, self) => {
    const fenceToken = tokens[idx];
    const info = fenceToken.info ? markdownit.utils.unescapeAll(fenceToken.info).trim() : '';
    const lang = info.split(/(\s+)/g)[0];

    const _selectLanguage = selectLanguage(options, lang),
          _selectLanguage2 = _slicedToArray(_selectLanguage, 1),
          langToUse = _selectLanguage2[0];

    if (!langToUse) {
      return existingRule(tokens, idx, renderOptions, env, self);
    } else {
      fenceToken.info = langToUse;
      const existingResult = existingRule(tokens, idx, renderOptions, env, self);
      const langClass = languageClass(markdownit, markdownit.utils.escapeHtml(langToUse));
      return existingResult.replace(/<((?:pre|code)[^>]*?)(?:\s+class="([^"]*)"([^>]*))?>/g, (match, tagStart, existingClasses, tagEnd) => existingClasses !== null && existingClasses !== void 0 && existingClasses.includes(langClass) ? match : `<${tagStart} class="${existingClasses ? `${existingClasses} ` : ''}${langClass}"${tagEnd || ''}>`);
    }
  };
}
/**
 * Renders inline code tokens by highlighting them with Prism.
 *
 * @param markdownit
 *        The markdown-it instance.
 * @param options
 *        The options that have been used to initialise the plugin.
 * @param existingRule
 *        The previously configured render rule for inline code.
 */


function renderInlineCode(markdownit, options, existingRule) {
  return (tokens, idx, renderOptions, env, self) => {
    const inlineCodeToken = tokens[idx];
    const specifiedLanguage = inlineCodeToken.meta ? inlineCodeToken.meta[SPECIFIED_LANGUAGE_META_KEY] || '' : '';

    const _selectLanguage3 = selectLanguage(options, specifiedLanguage),
          _selectLanguage4 = _slicedToArray(_selectLanguage3, 2),
          langToUse = _selectLanguage4[0],
          prismLang = _selectLanguage4[1];

    if (!langToUse) {
      return existingRule(tokens, idx, renderOptions, env, self);
    } else {
      const highlighted = highlightWithSelectedLanguage(markdownit, options, inlineCodeToken.content, [langToUse, prismLang]);
      inlineCodeToken.attrJoin('class', languageClass(markdownit, langToUse));
      return `<code${self.renderAttrs(inlineCodeToken)}>${highlighted}</code>`;
    }
  };
}
/**
 * Checks whether an option represents a valid Prism language
 *
 * @param options
 *        The options that have been used to initialise the plugin.
 * @param optionName
 *        The key of the option inside {@code options} that shall be checked.
 * @throws {Error} If the option is not set to a valid Prism language.
 */


function checkLanguageOption(options, optionName) {
  const language = options[optionName];

  if (language !== undefined && loadPrismLang(language) === undefined) {
    throw new Error(`Bad option ${optionName}: There is no Prism language '${language}'.`);
  }
}
/**
 * ‘the most basic rule to render a token’ (https://github.com/markdown-it/markdown-it/blob/master/docs/examples/renderer_rules.md)
 */


function renderFallback(tokens, idx, options, env, self) {
  return self.renderToken(tokens, idx, options);
}
/**
 * Initialisation function of the plugin. This function is not called directly by clients, but is rather provided
 * to MarkdownIt’s {@link MarkdownIt.use} function.
 *
 * @param markdownit
 *        The markdown it instance the plugin is being registered to.
 * @param useroptions
 *        The options this plugin is being initialised with.
 */


function markdownItPrism(markdownit, useroptions) {
  const options = Object.assign({}, DEFAULTS, useroptions);
  checkLanguageOption(options, 'defaultLanguage');
  checkLanguageOption(options, 'defaultLanguageForUnknown');
  checkLanguageOption(options, 'defaultLanguageForUnspecified');
  options.defaultLanguageForUnknown = options.defaultLanguageForUnknown || options.defaultLanguage;
  options.defaultLanguageForUnspecified = options.defaultLanguageForUnspecified || options.defaultLanguage;
  options.plugins.forEach(loadPrismPlugin);
  options.init(_prismjs.default); // register ourselves as highlighter

  markdownit.options.highlight = (text, lang) => highlight(markdownit, options, text, lang);

  markdownit.renderer.rules.fence = applyCodeAttributes(markdownit, options, markdownit.renderer.rules.fence || renderFallback);

  if (options.highlightInlineCode) {
    markdownit.core.ruler.after('inline', 'prism_inline_code_language', inlineCodeLanguageRule);
    markdownit.renderer.rules.code_inline = renderInlineCode(markdownit, options, markdownit.renderer.rules.code_inline || renderFallback);
  }
}

module.exports = exports.default;
module.exports.default = exports.default;