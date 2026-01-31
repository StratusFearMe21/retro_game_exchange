window.htmx = require('htmx.org').default;
const swal = require('sweetalert');

document.body.addEventListener("htmx:configRequest", function(evt) {
  for (const key of Reflect.ownKeys(evt.detail.parameters)) {
    if (evt.detail.parameters[key] === "") {
      delete evt.detail.parameters[key];
    }
  }
});

document.body.addEventListener("htmx:beforeRequest", function(evt) {
  if (evt.detail.elt !== document.body) {
    const elt = window.htmx.find(evt.detail.elt, ':not(input, select, textarea, html, form)')
    elt.ariaBusy = true;
  }
});

document.body.addEventListener("htmx:beforeOnLoad", function(evt) {
  if (evt.detail.elt !== document.body) {
    const elt = window.htmx.find(evt.detail.elt, ':not(input, select, textarea, html, form)')
    elt.ariaBusy = false;
  }
});

document.body.addEventListener("htmx:beforeSwap", function(evt) {
  const contentType = evt.detail.xhr.getResponseHeader("Content-Type");

  if (contentType === "application/json") {
    const swalParams = JSON.parse(evt.detail.serverResponse)
    evt.preventDefault()

    if (swalParams.content != undefined) {
      const elt = document.createElement('div')
      elt.innerHTML = swalParams.content
      swalParams.content = elt
    }

    swal(
      swalParams
    )
    .then((value) => {
      switch (value) {
        case "sign_out":
          window.htmx.ajax('GET', '/auth/logout');
          break;
       }
    });
  }
});
