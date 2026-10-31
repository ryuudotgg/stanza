import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        Object.assign(config.rules, strict);
        export { config, clean }; export default clean;