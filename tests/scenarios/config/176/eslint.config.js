import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        Reflect.set(config.rules, "curly", "error");
        export { config, clean }; export default clean;