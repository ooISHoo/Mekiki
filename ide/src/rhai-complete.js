// Rhai 予約語と Mekiki API の補完。
//
// API の正本は api/rhai-api.toml。この生成済みカタログは名前だけでなく、
// 補完の説明パネルに表示する署名と契約も提供する。

import {
  autocompletion,
  completeFromList,
  completionKeymap,
  ifNotIn,
} from "@codemirror/autocomplete";

import catalog from "./mekiki-api.generated.json";
import { t } from "./i18n/index.js";
import { renderApiCompletionInfo } from "./rhai-api-info.js";

// 公式 Keywords 表の「実際に使える語」（Active）。
// goto / async など予約されているだけの語は候補に出さない。
// https://rhai.rs/book/language/keywords.html
const RHAI_KEYWORDS = [
  "true",
  "false",
  "let",
  "const",
  "if",
  "else",
  "switch",
  "do",
  "while",
  "loop",
  "until",
  "for",
  "in",
  "continue",
  "break",
  "fn",
  "private",
  "this",
  "return",
  "throw",
  "try",
  "catch",
  "import",
  "export",
  "as",
  "global",
];

const RHAI_BUILTINS = [
  "print",
  "debug",
  "eval",
  "type_of",
  "is_def_fn",
  "is_def_var",
  "is_shared",
  "Fn",
  "call",
  "curry",
];

const IN_STRING_OR_COMMENT = [
  "LineComment",
  "BlockComment",
  "String",
  "TemplateString",
];

const PROPERTY_NAMES = new Set(
  catalog.items
    .filter((item) => item.kind === "property")
    .map((item) => item.name),
);

const HIDDEN_MEMBERS = new Set(["to_string", "to_debug"]);

const keywordOptions = [
  ...RHAI_KEYWORDS.map((label) => ({ label, type: "keyword" })),
  ...RHAI_BUILTINS.map((label) => ({ label, type: "function" })),
];

function apiInfoLabels() {
  return {
    global: t("apiInfo.global"),
    parameters: t("apiInfo.parameters"),
    constraints: t("apiInfo.constraints"),
    errors: t("apiInfo.errors"),
    examples: t("apiInfo.examples"),
  };
}

const globalOptions = catalog.globals.map((label) => ({
  label,
  type: "function",
  info: () =>
    renderApiCompletionInfo(catalog.items, label, "global", apiInfoLabels()),
}));

const memberOptions = uniqueMembers().flatMap((label) => {
  if (HIDDEN_MEMBERS.has(label)) return [];
  return [
    {
      label,
      type: PROPERTY_NAMES.has(label) ? "property" : "method",
      info: () =>
        renderApiCompletionInfo(catalog.items, label, "member", apiInfoLabels()),
    },
  ];
});

function uniqueMembers() {
  const names = new Set();
  for (const list of Object.values(catalog.members)) {
    for (const name of list) names.add(name);
  }
  return [...names].sort();
}

const keywordSource = ifNotIn(IN_STRING_OR_COMMENT, completeFromList(keywordOptions));

function globalSource(context) {
  const match = context.matchBefore(/\w*$/);
  if (!match) return null;
  if (match.from === match.to && !context.explicit) return null;
  if (match.from > 0 && context.state.sliceDoc(match.from - 1, match.from) === ".") {
    return null;
  }
  return {
    from: match.from,
    options: globalOptions,
    validFor: /^\w*$/,
  };
}

function memberSource(context) {
  const match = context.matchBefore(/\.\w*$/);
  if (!match) return null;
  return {
    from: match.from + 1,
    options: memberOptions,
    validFor: /^\w*$/,
  };
}

/// エディタへ足す拡張。keymap は呼び出し側で他のマップと並べる。
export function rhaiCompletion() {
  return autocompletion({
    override: [
      ifNotIn(IN_STRING_OR_COMMENT, globalSource),
      ifNotIn(IN_STRING_OR_COMMENT, memberSource),
      keywordSource,
    ],
    activateOnTyping: true,
  });
}

export { completionKeymap };
