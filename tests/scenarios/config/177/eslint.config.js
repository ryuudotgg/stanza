import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        Reflect.get(config.rules, "curly");
        export { config, clean }; export default clean;