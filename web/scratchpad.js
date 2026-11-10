/* Scratchpad with KaTeX rendering and a simple cylinder diagram.
 * KaTeX is loaded from CDN when online; offline falls back to plain text
 * wrapped in a .katex-fallback span so the layout still works.
 */
function hasKatex() {
  return typeof window.katex !== "undefined" && window.katex && typeof window.katex.renderToString === "function";
}

function renderLatexToHtml(latex) {
  if (hasKatex()) {
    try {
      return window.katex.renderToString(latex, { throwOnError: false });
    } catch (e) {
      return "";
    }
  }
  return "";
}

function previewInto(targetId, latex) {
  const target = document.getElementById(targetId);
  if (!target) return;
  const html = renderLatexToHtml(latex);
  if (html) {
    target.innerHTML = html;
  } else {
    const span = document.createElement("span");
    span.className = "katex-fallback";
    span.textContent = latex;
    target.innerHTML = "";
    target.appendChild(span);
  }
}

function bindScratchpad(inputId, previewId) {
  const input = document.getElementById(inputId || "scratch-input");
  const preview = previewId || "scratch-preview";
  if (!input) return;
  input.addEventListener("input", () => previewInto(preview, input.value));
}

function drawCylinder(targetId) {
  const target = document.getElementById(targetId);
  if (!target) return;
  // Simple inline SVG: cylinder with radius and height labels.
  target.innerHTML =
    '<svg class="diagram" viewBox="0 0 200 160" role="img" aria-label="Cylinder diagram">' +
    '<ellipse cx="100" cy="40" rx="52" ry="16" fill="none" stroke="currentColor" stroke-width="2"/>' +
    '<line x1="48" y1="40" x2="48" y2="120" stroke="currentColor" stroke-width="2"/>' +
    '<line x1="152" y1="40" x2="152" y2="120" stroke="currentColor" stroke-width="2"/>' +
    '<ellipse cx="100" cy="120" rx="52" ry="16" fill="none" stroke="currentColor" stroke-width="2" stroke-dasharray="6 4"/>' +
    '<line x1="100" y1="40" x2="152" y2="40" stroke="currentColor" stroke-width="1" stroke-dasharray="4 3"/>' +
    '<text x="118" y="36" fill="currentColor" font-size="12">r</text>' +
    '<text x="158" y="85" fill="currentColor" font-size="12">h</text>' +
    "</svg>";
}

document.addEventListener("DOMContentLoaded", () => {
  bindScratchpad("scratch-input", "scratch-preview");
  drawCylinder("scratch-diagram");
});

window.BreadstudyScratch = { previewInto, bindScratchpad, drawCylinder, renderLatexToHtml };
