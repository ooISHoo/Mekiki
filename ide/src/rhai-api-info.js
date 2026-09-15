function apiOwner(item) {
  return item.owner || "";
}

export function formatApiSignature(item) {
  const owner = item.owner ? `${item.owner}.` : "";
  if (item.kind === "property") {
    return `${owner}${item.name} -> ${item.returns}`;
  }
  const params = item.params
    .map((param) => `${param.name}: ${param.type}`)
    .join(", ");
  return `${owner}${item.name}(${params}) -> ${item.returns}`;
}

export function buildApiInfoModel(items, name, scope) {
  const matches = items.filter((item) => {
    if (item.name !== name) return false;
    return scope === "global" ? !item.owner : Boolean(item.owner);
  });

  const groups = new Map();
  for (const item of matches) {
    const owner = apiOwner(item);
    if (!groups.has(owner)) groups.set(owner, []);
    groups.get(owner).push({
      ...item,
      signature: formatApiSignature(item),
    });
  }

  return [...groups.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([owner, overloads]) => ({ owner, overloads }));
}

function appendText(parent, className, text) {
  if (!text) return;
  const element = document.createElement("p");
  element.className = className;
  element.textContent = text;
  parent.append(element);
}

function appendList(parent, title, entries, className = "") {
  if (!entries.length) return;

  const section = document.createElement("section");
  section.className = `mekiki-api-info-section ${className}`.trim();
  const heading = document.createElement("h4");
  heading.textContent = title;
  section.append(heading);

  const list = document.createElement("ul");
  for (const entry of entries) {
    const item = document.createElement("li");
    item.textContent = entry;
    list.append(item);
  }
  section.append(list);
  parent.append(section);
}

function appendParameters(parent, params, labels) {
  if (!params.some((param) => param.description)) return;
  const entries = params
    .filter((param) => param.description)
    .map((param) => `${param.name}: ${param.description}`);
  appendList(parent, labels.parameters, entries);
}

function renderOverload(item, labels) {
  const article = document.createElement("article");
  article.className = "mekiki-api-info-overload";

  const signature = document.createElement("code");
  signature.className = "mekiki-api-info-signature";
  signature.textContent = item.signature;
  article.append(signature);

  appendText(article, "mekiki-api-info-summary", item.summary);
  appendText(article, "mekiki-api-info-details", item.details);
  appendParameters(article, item.params, labels);
  appendList(article, labels.constraints, item.constraints);
  appendList(article, labels.errors, item.errors, "is-error");
  appendList(article, labels.examples, item.examples);
  return article;
}

export function renderApiCompletionInfo(items, name, scope, labels) {
  const groups = buildApiInfoModel(items, name, scope);
  const root = document.createElement("div");
  root.className = "mekiki-api-info";

  for (const group of groups) {
    const section = document.createElement("section");
    section.className = "mekiki-api-info-group";

    if (scope === "member" || groups.length > 1) {
      const heading = document.createElement("h3");
      heading.textContent = group.owner || labels.global;
      section.append(heading);
    }

    for (const overload of group.overloads) {
      section.append(renderOverload(overload, labels));
    }
    root.append(section);
  }

  return root;
}
