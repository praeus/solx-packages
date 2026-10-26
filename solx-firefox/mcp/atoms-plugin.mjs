// Rewrites selenium-webdriver's WebDriver atom loading for `bun build --compile`.
//
// selenium-webdriver/lib/http.js loads three "atoms" (injected-source helpers
// for getAttribute/isDisplayed/findElements) through a dynamic
//
//     require('./atoms/' + module)
//
// and calls it at module scope, through requireAtom(). A bundler cannot resolve
// that: the compiled binary keeps the raw `require`, and at run time it looks
// for ./atoms/... inside the virtual filesystem ($bunfs/root) and throws
//
//     Cannot find module './atoms/get-attribute.js'
//
// before the server has even parsed --version. (Node does not hit this because
// it resolves the path against the real node_modules directory on disk.)
//
// This plugin replaces the three call sites with static requires during
// onLoad, so Bun bundles the atom files and requireAtom's fallback -- the
// bazel-bin path, which is dev-only -- is never reached.
// This is a plain plugin object rather than `plugin()` from "bun": `plugin()`
// is the bunfig.toml form, and Bun.build's `plugins: []` rejects it as
// "Expected plugin to be an object". Both hooks are identical.
const ATOMS = ["get-attribute.js", "is-displayed.js", "find-elements.js"];

export default {
  name: "selenium-webdriver-atoms",
  setup(build) {
    build.onLoad(
      { filter: /[\\/]selenium-webdriver[\\/]lib[\\/]http\.js$/ },
      async (args) => {
        let src = await Bun.file(args.path).text();
        for (const atom of ATOMS) {
          // requireAtom('<file>', '<bazel target>') -> require('./atoms/<file>')
          const call = new RegExp(
            `requireAtom\\(\\s*['"]${atom.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}['"][^)]*\\)`,
          );
          src = src.replace(call, `require("./atoms/${atom}")`);
        }
        return { contents: src, loader: "js" };
      },
    );
  },
};
