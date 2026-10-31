import plugin from "@demo/eslint-plugin";
          export default [plugin.configs.recommended, ...plugin.configs.recommended, plugin.configs[unknownName],
            { ...plugin.configs.recommended, rules: { ...plugin.configs.recommended.rules } },
            { plugins: { demo: plugin }, extends: ["demo/recommended"] }];