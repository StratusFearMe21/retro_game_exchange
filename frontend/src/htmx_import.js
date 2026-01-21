window.htmx = require('htmx.org');
swal = require('sweetalert')

document.body.addEventListener("htmx:configRequest", function(evt) {
  for (const key of Reflect.ownKeys(evt.detail.parameters)) {
    console.log(evt.detail.parameters[key])
    if (evt.detail.parameters[key] === "") {
      delete evt.detail.parameters[key];
    }
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
          window.htmx.default.ajax('GET', '/auth/logout');
          break;
       }
    });
  }
});
