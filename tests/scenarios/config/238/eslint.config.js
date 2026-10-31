const { defineConfig } = require("eslint/config");
module.exports = defineConfig([{ rules: { curly: "off" } }, { ignores: ["dist/*"] }]);