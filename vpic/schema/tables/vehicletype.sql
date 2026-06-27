CREATE TABLE vpic.vehicletype (
    id integer NOT NULL,
    name character varying(250) NOT NULL,
    displayorder integer,
    formtype integer,
    description character varying(4000),
    includeinequipplant boolean
);
