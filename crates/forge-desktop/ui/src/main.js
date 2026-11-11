import { mount } from "svelte";
import "@fontsource-variable/geist/wght.css";
import "@fontsource-variable/geist-mono/wght.css";
import "./app.css";
import App from "./App.svelte";

mount(App, { target: document.getElementById("app") });
