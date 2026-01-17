window.htmx = require('htmx.org');

document.body.addEventListener("htmx:configRequest", function(evt) {
  for (const key of Reflect.ownKeys(evt.detail.parameters)) {
    console.log(evt.detail.parameters[key])
    if (evt.detail.parameters[key] === "") {
      delete evt.detail.parameters[key];
    }
  }
});
