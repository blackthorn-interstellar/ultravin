CREATE TABLE vpic.vehiclespecschema (
    id integer NOT NULL,
    makeid integer NOT NULL,
    createdon timestamp without time zone NOT NULL,
    updatedon timestamp without time zone,
    vehicletypeid integer,
    sourcedate timestamp without time zone,
    tobeqced boolean
);
