import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        for (const item of [config]) item.rules.curly = "error";
        export { config, clean }; export default clean;