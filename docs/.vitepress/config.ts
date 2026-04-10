import { defineConfig } from "vitepress";

export default defineConfig({
  title: "multilinguarr",
  description: "Multi-language audio enforcement for the *arr media stack",

  base: "/multilinguarr/",
  cleanUrls: true,
  lastUpdated: true,

  sitemap: {
    hostname: "https://dlepaux.github.io/multilinguarr",
  },

  vue: {
    template: {
      compilerOptions: {
        isCustomElement: (tag) => tag === "scalar-api-reference",
      },
    },
  },

  head: [
    ["meta", { name: "theme-color", content: "#e05db5" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:locale", content: "en" }],
    ["meta", { property: "og:title", content: "multilinguarr" }],
    [
      "meta",
      {
        property: "og:description",
        content: "Multi-language audio enforcement for the *arr media stack",
      },
    ],
    ["meta", { property: "og:site_name", content: "multilinguarr" }],
    [
      "meta",
      {
        property: "og:url",
        content: "https://dlepaux.github.io/multilinguarr/",
      },
    ],
    [
      "meta",
      {
        property: "og:image",
        content: "https://dlepaux.github.io/multilinguarr/og-share.png",
      },
    ],
    ["meta", { property: "og:image:width", content: "1280" }],
    ["meta", { property: "og:image:height", content: "640" }],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
    [
      "meta",
      {
        name: "twitter:image",
        content: "https://dlepaux.github.io/multilinguarr/og-share.png",
      },
    ],
    [
      "script",
      { type: "application/ld+json" },
      JSON.stringify({
        "@context": "https://schema.org",
        "@type": "SoftwareSourceCode",
        name: "multilinguarr",
        description:
          "Multi-language audio enforcement for the *arr media stack",
        url: "https://github.com/dlepaux/multilinguarr",
        codeRepository: "https://github.com/dlepaux/multilinguarr",
        programmingLanguage: "Rust",
        license: "https://opensource.org/licenses/MIT",
        author: {
          "@type": "Person",
          name: "David Lepaux",
          url: "https://github.com/dlepaux",
        },
      }),
    ],
    [
      "link",
      {
        rel: "icon",
        type: "image/svg+xml",
        href: "/multilinguarr/favicon/favicon.svg",
      },
    ],
    [
      "link",
      {
        rel: "icon",
        type: "image/png",
        sizes: "96x96",
        href: "/multilinguarr/favicon/favicon-96x96.png",
      },
    ],
    [
      "link",
      { rel: "shortcut icon", href: "/multilinguarr/favicon/favicon.ico" },
    ],
    [
      "link",
      {
        rel: "apple-touch-icon",
        sizes: "180x180",
        href: "/multilinguarr/favicon/apple-touch-icon.png",
      },
    ],
    [
      "link",
      { rel: "manifest", href: "/multilinguarr/favicon/site.webmanifest" },
    ],
  ],

  themeConfig: {
    logo: "/logo.svg",

    nav: [
      { text: "Guide", link: "/guide/introduction" },
      { text: "API Reference", link: "/api/" },
      {
        text: "More",
        items: [
          { text: "Contributing", link: "/contributing" },
          { text: "License", link: "/license" },
        ],
      },
    ],

    sidebar: {
      "/guide/": [
        {
          text: "Getting Started",
          items: [
            { text: "Introduction", link: "/guide/introduction" },
            { text: "Installation", link: "/guide/installation" },
            { text: "Directory Structure", link: "/guide/directory-structure" },
            { text: "Configuration", link: "/guide/configuration" },
          ],
        },
        {
          text: "Concepts",
          items: [
            { text: "How It Works", link: "/guide/how-it-works" },
            { text: "Symlinks vs Hardlinks", link: "/guide/links" },
            { text: "Migration Guide", link: "/guide/migration" },
          ],
        },
      ],
    },

    socialLinks: [
      { icon: "github", link: "https://github.com/dlepaux/multilinguarr" },
    ],

    editLink: {
      pattern: "https://github.com/dlepaux/multilinguarr/edit/main/docs/:path",
    },

    search: {
      provider: "local",
    },

    footer: {
      message:
        'Released under the <a href="https://github.com/dlepaux/multilinguarr/blob/main/license.md">MIT License</a>.',
      copyright: "Copyright &copy; 2026 David Lepaux",
    },
  },
});
