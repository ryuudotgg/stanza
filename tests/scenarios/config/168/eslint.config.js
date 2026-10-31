import strict from "./strict.js";
        const config = { rules: { curly: "off" } };
        const clean = { rules: { curly: "off" } };
        function change(item) { item.rules.curly = "error"; }
        export { config, clean }; export default clean;