/* GridFPV — diagram bootstrap for internal design docs.
   Renders <pre class="mermaid"> blocks using the vendored Mermaid build.
   Self-contained: Mermaid is vendored in assets/vendor/ so this works offline
   over file:// with no CDN. Load order in each page:
     <script src="assets/vendor/mermaid.min.js"></script>
     <script src="assets/diagrams.js"></script>
   Place both at the end of <body> so the diagram elements already exist. */

(function () {
  if (typeof window.mermaid === "undefined") {
    console.error("[GridFPV] Mermaid not loaded — check assets/vendor/mermaid.min.js");
    return;
  }

  // Theme tuned to match assets/style.css (brand green + the document palette).
  window.mermaid.initialize({
    startOnLoad: false,
    securityLevel: "loose", // trusted, internal-only docs; allows <br/> in labels
    theme: "base",
    themeVariables: {
      fontFamily:
        '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      fontSize: "14px",
      primaryColor: "#f6f8fa", // node fill  (--surface)
      primaryBorderColor: "#43b301", // node border (--gfpv-green)
      primaryTextColor: "#1c2025", // node text  (--text)
      lineColor: "#5b6671", // edges      (--text-muted)
      secondaryColor: "#eef1f4", // (--surface-2)
      tertiaryColor: "#ffffff", // (--bg)
      clusterBkg: "#ffffff",
      clusterBorder: "#d8dee4", // (--border)
      titleColor: "#357f06", // subgraph titles (--gfpv-green-dark)
    },
    flowchart: { curve: "basis", htmlLabels: true, padding: 12 },
  });

  // Scripts sit at end of <body>, so the DOM is already parsed — render now.
  window.mermaid.run({ querySelector: "pre.mermaid" });
})();
