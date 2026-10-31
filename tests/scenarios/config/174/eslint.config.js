import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        Object.defineProperties(config.rules, { curly: { value: "error" } });
        export { config, clean }; export default clean;