const getCurrentDate = () => {
  const t = new Date();
  const date = ("0" + t.getDate()).slice(-2);
  const month = ("0" + (t.getMonth() + 1)).slice(-2);
  const year = t.getFullYear();
  return `${date}/${month}/${year}`;
};

// See
// https://docs.aws.amazon.com/iot/latest/developerguide/pre-provisioning-hook.html
// for the full documentation

// Pre-provision hook input
//
// {
//   "claimCertificateId" : "string",
//   "certificateId" : "string",
//   "certificatePem" : "string",
//   "templateArn" : "arn:aws:iot:us-east-1:1234567890:provisioningtemplate/MyTemplate",
//   "clientId" : "221a6d10-9c7f-42f1-9153-e52e6fc869c1",
//   "parameters" : {
//       "string" : "string",
//       ...
//   }
// }
//
// The parameters object passed to the Lambda function contains the properties
// in the parameters argument passed in the RegisterThing request payload.


// Pre-provision hook return value
//
// {
//   "allowProvisioning": true,
//   "parameterOverrides" : {
//       "Key": "newCustomValue",
//       ...
//   }
// }
//
// "parameterOverrides" values will be added to "parameters" parameter of the RegisterThing request payload.

export async function handler(event) {
  const provision_response = {
    allowProvisioning: true,
    parameterOverrides: {
      CertDate: getCurrentDate(),
    },
  };

  // Stringent validation against internal API's/DB etc to validate the request before proceeding
  //
  // if ((event.parameters.SerialNumber = "approved by company CSO")) {
  //   provision_response.allowProvisioning = true;
  // }

  // It is also possible here to revoke/delete any previously active
  // certificates attached to the thing. Use the SDK for this.

  return provision_response;
}
